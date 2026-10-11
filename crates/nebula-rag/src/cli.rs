//! `nebula rag …`. On the RAG host (findl) it opens `rag.db` directly;
//! anywhere else it re-runs itself as `ssh <host> nebula rag …`, the same
//! BatchMode hop `nebula remote` uses — the SSH key is the auth.

use crate::apps::origin_of;
use crate::embed::{self, Embedded, Embedder, WorkersAi};
use crate::obs::{since_cutoff, Category, Obs, ObsFilter, Status, Transition};
use crate::store::{age, Hit, Kind, NewDoc, NoteAdded, Rag};
use crate::ws;
use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use std::cell::OnceCell;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Default RAG host; `NEBULA_RAG_HOST` overrides.
pub const DEFAULT_HOST: &str = "findl";
/// Files and URLs larger than this are refused.
const MAX_DOC_BYTES: usize = 1 << 20;

#[derive(Args, Debug)]
pub struct RagArgs {
    /// Who is writing (stored on notes). Default: the nebula session
    /// (`NEBULA_AGENT_ID`), else `cli@<host>`.
    #[arg(long)]
    pub source: Option<String>,
    /// Git remote of the caller's checkout, for the default scope. Set by
    /// the ssh hop; locally it comes from the current directory.
    #[arg(long, hide = true)]
    pub origin: Option<String>,
    #[command(subcommand)]
    pub command: RagCommand,
}

#[derive(Subcommand, Debug)]
pub enum RagCommand {
    /// Semantic search (default scope: this repo's app + shared). A query
    /// in double quotes, or --exact, is an exact-term (FTS5) search.
    Search {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Workspace path(s) to search, comma-separated; `/` = everything.
        #[arg(long, value_delimiter = ',')]
        ws: Vec<String>,
        /// Number of hits.
        #[arg(short, default_value_t = 5)]
        k: usize,
        #[arg(long)]
        json: bool,
        /// Exact terms (FTS5) instead of semantic ranking.
        #[arg(long)]
        exact: bool,
    },
    /// Save a note ("<text>"), or index a file or URL. Notes dedup: one at
    /// ≥ 0.9 similarity in the same workspace is updated instead.
    Add {
        /// Note text (words are joined), a file path, an http(s) URL, or `-`
        /// for stdin.
        #[arg(required = true, num_args = 1..)]
        target: Vec<String>,
        /// Workspace. Notes default to apps/<app>/notes inside an app's
        /// checkout; files and URLs always need it.
        #[arg(long)]
        ws: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Always create a new note, even when a near-identical one exists.
        #[arg(long)]
        no_dedup: bool,
        /// Stdin is a file with this name (what the ssh hop sends for a
        /// local file).
        #[arg(long, hide = true)]
        name: Option<String>,
    },
    /// The workspace tree.
    Ws {
        #[command(subcommand)]
        command: WsCommand,
    },
    /// List, read, fix, move or delete notes.
    Note {
        #[command(subcommand)]
        command: NoteCommand,
    },
    /// Observations: things that went wrong while an agent worked, each
    /// with a category and a status (open, done, dismissed). Never deduped.
    Obs {
        #[command(subcommand)]
        command: ObsCommand,
    },
    /// Repos whose markdown on origin/main is indexed.
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
    /// Fetch and re-index apps' markdown from origin/<branch> (all apps, or
    /// one), and purge notes deleted over 30 days ago. With no app, the
    /// host's nebula projects that have an origin and no app are registered first.
    Sync {
        app: Option<String>,
        /// Reconcile every file instead of diffing from the last commit.
        #[arg(long)]
        full: bool,
    },
    /// Write the RAG host's systemd user units (sync timer every 10 min,
    /// Litestream → R2) and download the pinned Litestream. Does not start
    /// them unless --enable.
    Setup {
        /// R2 key prefix for the replica. Only one replicator may write it.
        #[arg(long, default_value = crate::setup::DEFAULT_PREFIX)]
        prefix: String,
        /// Print the files instead of writing them.
        #[arg(long)]
        dry_run: bool,
        /// Also `systemctl --user enable --now` the timer and Litestream.
        #[arg(long)]
        enable: bool,
        /// Skip downloading Litestream.
        #[arg(long)]
        no_download: bool,
        /// nebula binary the units run (default: the installed one).
        #[arg(long)]
        exe: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum WsCommand {
    /// Tree with document/note counts.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a workspace (and missing parents). Refuses a name close to an
    /// existing sibling and suggests it.
    Create {
        path: String,
        #[arg(long)]
        force: bool,
    },
    /// Remove a workspace; a non-empty one needs --force (deletes its
    /// documents and sub-workspaces).
    Rm {
        path: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum NoteCommand {
    /// Notes, newest first (default scope: this repo's app + shared).
    List {
        #[arg(long, value_delimiter = ',')]
        ws: Vec<String>,
        /// Soft-deleted notes instead (purged 30 days after deletion).
        #[arg(long)]
        deleted: bool,
        #[arg(long)]
        json: bool,
    },
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Replace a note's text (--text, or stdin / $EDITOR) and/or title.
    Edit {
        id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        title: Option<String>,
    },
    /// Soft-delete (out of search now, purged after 30 days).
    Rm { id: String },
    /// Move a note to another (existing) workspace.
    Mv { id: String, ws: String },
}

#[derive(Subcommand, Debug)]
pub enum ObsCommand {
    /// Record one ("<text>"). The same event saved again is a new row.
    Add {
        /// What went wrong (words are joined).
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// correction, rework, search, skill or tool.
        #[arg(long)]
        category: Category,
        /// Default: the first line of the text.
        #[arg(long)]
        title: Option<String>,
        /// App it belongs to (default: this checkout's app, else `shared`).
        #[arg(long)]
        app: Option<String>,
    },
    /// Observations, newest first, across every app and shared.
    List {
        /// open, done, dismissed or all.
        #[arg(long, default_value = "open")]
        status: StatusFilter,
        #[arg(long)]
        category: Option<Category>,
        /// One app, or `shared`.
        #[arg(long)]
        app: Option<String>,
        /// Created in the last 7d / 24h, or since a date (2026-10-01, UTC).
        #[arg(long, value_parser = since_arg)]
        since: Option<i64>,
        #[arg(long)]
        json: bool,
    },
    /// One observation in full, with its resolution once closed.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Close as fixed; --note says what was built.
    Done {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Close as not important; --note says why.
    Dismiss {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Back to open; the resolution is cleared.
    Reopen {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Counts by status, category and app: where the period went.
    Stats {
        /// Created in the last 7d / 24h, or since a date (2026-10-01, UTC).
        #[arg(long, value_parser = since_arg)]
        since: Option<i64>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct StatusFilter(Option<Status>);

impl std::str::FromStr for StatusFilter {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(Self(match s {
            "all" => None,
            s => Some(s.parse().map_err(|e| anyhow!("{e}, or all"))?),
        }))
    }
}

fn since_arg(s: &str) -> Result<i64> {
    since_cutoff(s, crate::db::now())
}

#[derive(Subcommand, Debug)]
pub enum AppCommand {
    /// Register a repo checkout on the RAG host; creates apps/<name>/{docs,notes}.
    Add {
        name: String,
        /// Path of the checkout on the RAG host.
        #[arg(long)]
        repo: PathBuf,
        /// Branch to index (default: origin's HEAD, else main).
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        force: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: RagArgs) -> Result<()> {
    if !is_local() {
        let code = proxy(&args)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if let RagCommand::Setup {
        prefix,
        dry_run,
        enable,
        no_download,
        exe,
    } = args.command
    {
        return crate::setup::run(crate::setup::Opts {
            prefix,
            dry_run,
            enable,
            download: !no_download,
            exe,
        });
    }
    let mut rag = Rag::open(&crate::db::db_path(), Box::new(LazyWorkersAi::default()))?;
    let origin = args
        .origin
        .clone()
        .or_else(|| std::env::current_dir().ok().and_then(|d| origin_of(&d)));
    let source = Some(args.source.clone().unwrap_or_else(default_source));
    // A repo not registered as an app still scopes to apps/<repo> (which
    // may not exist yet) + shared, as the spec's default says.
    let app = match &origin {
        Some(url) => rag.app_for_remote(url)?.or_else(|| {
            crate::apps::remote_key(url)
                .rsplit('/')
                .next()
                .map(str::to_string)
        }),
        None => None,
    };
    let ctx = Ctx { app, source };
    match args.command {
        RagCommand::Search {
            query,
            ws,
            k,
            json,
            exact,
        } => search(&rag, &ctx, &query.join(" "), ws, k, json, exact),
        RagCommand::Add {
            target,
            ws,
            title,
            no_dedup,
            name,
        } => add(&mut rag, &ctx, target, ws, title, no_dedup, name),
        RagCommand::Ws { command } => ws_cmd(&mut rag, command),
        RagCommand::Note { command } => note_cmd(&mut rag, &ctx, command),
        RagCommand::Obs { command } => obs_cmd(&mut rag, &ctx, command),
        RagCommand::App { command } => app_cmd(&rag, command),
        RagCommand::Sync { app, full } => crate::sync::run(&mut rag, app.as_deref(), full),
        RagCommand::Setup { .. } => unreachable!("handled above"),
    }
}

struct Ctx {
    app: Option<String>,
    source: Option<String>,
}

impl Ctx {
    /// `--ws` if given, else this app + shared, else everything.
    fn scope(&self, ws: Vec<String>) -> Vec<String> {
        if !ws.is_empty() {
            return ws;
        }
        match &self.app {
            Some(app) => vec![format!("apps/{app}"), "shared".into()],
            None => vec!["/".into()],
        }
    }
}

fn default_source() -> String {
    match std::env::var("NEBULA_AGENT_ID") {
        Ok(id) if !id.is_empty() => format!("nebula:{id}"),
        _ => format!("cli@{}", hostname()),
    }
}

fn rag_host() -> String {
    std::env::var("NEBULA_RAG_HOST")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.into())
}

/// Run against a local `rag.db`: forced by `NEBULA_RAG_LOCAL=1`, or this is
/// the RAG host — by name, or because its credentials (rag.env, which only
/// the RAG host has) are here. A stray local rag.db does not count.
fn is_local() -> bool {
    if let Ok(v) = std::env::var("NEBULA_RAG_LOCAL") {
        return v == "1";
    }
    embed::env_path().exists() || hostname() == rag_host()
}

fn hostname() -> String {
    nebula_core::host::hostname()
}

// ---- ssh hop ----

/// `ssh <host> nebula rag <args…>`, output streamed. A local file given to
/// `add` travels on stdin, since the host can't read the laptop's disk.
fn proxy(args: &RagArgs) -> Result<i32> {
    let host = rag_host();
    let mut words: Vec<String> = vec!["nebula".into(), "rag".into()];
    if let Some(o) = std::env::current_dir().ok().and_then(|d| origin_of(&d)) {
        words.extend(["--origin".into(), o]);
    }
    let source = args.source.clone().unwrap_or_else(default_source);
    words.extend(["--source".into(), source]);
    let mut rest: Vec<String> = std::env::args()
        .skip_while(|a| a != "rag")
        .skip(1)
        .collect();
    // Drop our own copies of the globals; they were re-added above.
    strip_globals(&mut rest);
    let mut upload: Option<PathBuf> = None;
    if let RagCommand::Add {
        target, name: None, ..
    } = &args.command
    {
        if let [t] = target.as_slice() {
            let p = Path::new(t);
            if t != "-" && !is_url(t) && p.is_file() {
                if let Some(pos) = rest.iter().position(|a| a == t) {
                    rest[pos] = "-".into();
                    let base = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    rest.extend(["--name".into(), base]);
                    upload = Some(p.to_path_buf());
                }
            }
        }
    }
    let edit_from_stdin = matches!(
        &args.command,
        RagCommand::Note {
            command: NoteCommand::Edit {
                text: None,
                title: None,
                ..
            }
        }
    );
    if edit_from_stdin && std::io::stdin().is_terminal() {
        bail!("over ssh there is no $EDITOR: pass --text \"…\" or pipe the new text on stdin");
    }
    let reads_stdin = upload.is_some() || rest.iter().any(|a| a == "-") || edit_from_stdin;
    words.extend(rest);
    let script = format!(
        "export PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:$PATH\"; {}",
        nebula_core::remote::join_quoted(words.iter().map(String::as_str))
    );
    let mut cmd = Command::new("ssh");
    cmd.args(nebula_core::remote::SSH_BATCH_OPTS);
    if !reads_stdin {
        cmd.arg("-n");
    }
    cmd.args(["--", &host, &script]);
    let status = if let Some(path) = upload {
        let data = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let mut child = cmd.stdin(Stdio::piped()).spawn().context("run ssh")?;
        child.stdin.take().context("ssh stdin")?.write_all(&data)?;
        child.wait()?
    } else {
        cmd.status().context("run ssh")?
    };
    let code = status.code().unwrap_or(1);
    if code == 255 {
        eprintln!("nebula rag: {host} unreachable over ssh (the RAG lives there; NEBULA_RAG_HOST overrides)");
    }
    Ok(code)
}

/// Remove `--source`/`--origin` (both spellings) ahead of the subcommand;
/// anything after it — note text included — is left alone.
fn strip_globals(args: &mut Vec<String>) {
    loop {
        match args.first().map(String::as_str) {
            Some("--source" | "--origin") => {
                args.drain(..2.min(args.len()));
            }
            Some(a) if a.starts_with("--source=") || a.starts_with("--origin=") => {
                args.remove(0);
            }
            _ => return,
        }
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Workers AI client built on first use, so commands that never embed
/// (ws list, note show) work without credentials.
#[derive(Default)]
struct LazyWorkersAi(OnceCell<std::result::Result<WorkersAi, String>>);

impl Embedder for LazyWorkersAi {
    fn embed(&self, texts: &[String]) -> Result<Embedded> {
        match self
            .0
            .get_or_init(|| WorkersAi::from_env().map_err(|e| e.to_string()))
        {
            Ok(w) => w.embed(texts),
            Err(e) => Err(anyhow!("{e}")),
        }
    }
}

// ---- commands ----

fn resolve_scope(rag: &Rag, paths: &[String]) -> Result<Option<Vec<i64>>> {
    for p in paths {
        if let Some(n) = ws::normalize(p)? {
            if rag.ws_id(&n)?.is_none() {
                eprintln!("nebula rag: no workspace {n} (see `nebula rag ws list`)");
            }
        }
    }
    rag.subtree_ids(paths)
}

fn search(
    rag: &Rag,
    ctx: &Ctx,
    query: &str,
    ws: Vec<String>,
    k: usize,
    json: bool,
    exact: bool,
) -> Result<()> {
    let paths = ctx.scope(ws);
    let scope = resolve_scope(rag, &paths)?;
    let quoted =
        query.trim().len() >= 2 && query.trim().starts_with('"') && query.trim().ends_with('"');
    let hits = if exact || quoted {
        rag.search_exact(query, scope.as_deref(), k)?
    } else {
        rag.search(query, scope.as_deref(), k)?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }
    println!(
        "scope: {}{}",
        paths.join(", "),
        if exact || quoted { "  (exact)" } else { "" }
    );
    if hits.is_empty() {
        println!("no hits");
    }
    for (i, h) in hits.iter().enumerate() {
        print_hit(i + 1, h);
    }
    Ok(())
}

fn print_hit(n: usize, h: &Hit) {
    println!(
        "\n[{n}] {:.3}  {}  {}  · {}",
        h.score,
        h.ws,
        h.source,
        age(h.updated)
    );
    if h.kind == "note" {
        println!("    {}", h.title);
    }
    for line in h.text.lines() {
        println!("    {line}");
    }
}

fn add(
    rag: &mut Rag,
    ctx: &Ctx,
    target: Vec<String>,
    ws_arg: Option<String>,
    title: Option<String>,
    no_dedup: bool,
    name: Option<String>,
) -> Result<()> {
    let single = (target.len() == 1).then(|| target[0].clone());
    let default_ws = |leaf: &str| -> Result<String> {
        match (&ws_arg, &ctx.app) {
            (Some(w), _) => Ok(w.clone()),
            (None, Some(app)) => Ok(format!("apps/{app}/{leaf}")),
            (None, None) => bail!("pass --ws <path> (not inside a registered app's checkout; see `nebula rag ws list`)"),
        }
    };
    // A file (local path, or stdin with --name) or a URL: a document.
    let doc: Option<(Kind, String, String)> = match single.as_deref() {
        Some("-") if name.is_some() => Some((Kind::File, name.clone().unwrap(), read_stdin()?)),
        Some(u) if is_url(u) => Some((Kind::Url, u.to_string(), fetch_url(u)?)),
        Some(p) if Path::new(p).is_file() => {
            let bytes = std::fs::read(p).with_context(|| format!("read {p}"))?;
            let label = Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string());
            Some((Kind::File, label, text_of(bytes, p)?))
        }
        _ => None,
    };
    if let Some((kind, path, body)) = doc {
        // Files and URLs need an explicit workspace, and never an app's
        // docs/, which sync owns (it would clash with the repo's own files).
        let Some(ws_path) = ws_arg.clone() else {
            bail!("pass --ws <path> for a file or URL (e.g. shared/<topic> or apps/<app>/notes)");
        };
        let ws_id = rag.ws_require(&ws_path)?;
        let norm = rag.ws_path(ws_id)?;
        if rag
            .apps()?
            .iter()
            .any(|a| norm == format!("apps/{}/docs", a.name))
        {
            bail!("{norm} is filled by `nebula rag sync` from the repo; pick another workspace");
        }
        let title = title.unwrap_or_else(|| path.clone());
        let id = rag.index_doc(
            &NewDoc {
                ws_id,
                kind,
                app: None,
                path: Some(&path),
                title: &title,
                body: &body,
                blob: None,
                source: ctx.source.as_deref(),
            },
            &path,
        )?;
        println!(
            "indexed {} {path} → {} ({}:{id})",
            kind.as_str(),
            rag.ws_path(ws_id)?,
            kind.as_str()
        );
        return Ok(());
    }
    let text = if single.as_deref() == Some("-") {
        read_stdin()?
    } else {
        target.join(" ")
    };
    let ws_path = default_ws("notes")?;
    let ws_id = rag.ws_require(&ws_path)?;
    let ws_name = rag.ws_path(ws_id)?;
    match rag.add_note(
        ws_id,
        title.as_deref(),
        &text,
        ctx.source.as_deref(),
        !no_dedup,
    )? {
        NoteAdded::Created(id) => println!("saved note:{id} → {ws_name}"),
        NoteAdded::Updated(id, sim) => {
            println!("updated note:{id} → {ws_name} (similar {sim:.2} to the new text, which replaced it)")
        }
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    text_of(buf, "stdin")
}

/// UTF-8 text under the size cap; binary (NUL bytes) refused.
fn text_of(bytes: Vec<u8>, what: &str) -> Result<String> {
    if bytes.len() > MAX_DOC_BYTES {
        bail!(
            "{what}: {} bytes is over the {MAX_DOC_BYTES}-byte limit",
            bytes.len()
        );
    }
    if bytes.contains(&0) {
        bail!("{what}: looks binary");
    }
    String::from_utf8(bytes).map_err(|_| anyhow!("{what}: not UTF-8 text"))
}

fn fetch_url(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "30", "--max-filesize"])
        .arg(MAX_DOC_BYTES.to_string())
        .args(["-w", "\n%{content_type}", "--", url])
        .output()
        .context("run curl")?;
    if !out.status.success() {
        bail!(
            "fetch {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut bytes = out.stdout;
    let split = bytes.iter().rposition(|b| *b == b'\n').unwrap_or(0);
    let ctype = String::from_utf8_lossy(&bytes[split..])
        .trim()
        .to_lowercase();
    bytes.truncate(split);
    let text = text_of(bytes, url)?;
    Ok(if ctype.contains("html") {
        html_to_text(&text)
    } else {
        text
    })
}

/// Crude HTML → text: drop script/style, tags become breaks, a few
/// entities decoded, blank runs collapsed. Headings keep `#` so the
/// chunker still splits on them.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    // ASCII-only folding keeps byte offsets identical to `html`'s.
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < html.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            let skip_to = ["script", "style", "noscript"]
                .iter()
                .find(|t| rest.starts_with(&format!("<{t}")))
                .and_then(|t| rest.find(&format!("</{t}>")).map(|e| e + t.len() + 3));
            if let Some(n) = skip_to {
                i += n;
                continue;
            }
            let tag_end = rest.find('>').unwrap_or(rest.len());
            let tag = &rest[1..tag_end];
            let end = (tag_end + 1).min(rest.len());
            let hn = tag.as_bytes();
            if hn.len() >= 2
                && hn[0] == b'h'
                && (b'1'..=b'6').contains(&hn[1])
                && (hn.len() == 2 || !hn[2].is_ascii_alphanumeric())
            {
                out.push_str("\n\n");
                out.push_str(&"#".repeat((hn[1] - b'0') as usize));
                out.push(' ');
            } else if [
                "p", "div", "br", "li", "tr", "/h", "section", "article", "pre",
            ]
            .iter()
            .any(|t| tag.starts_with(t))
            {
                out.push('\n');
            }
            i += end;
            continue;
        }
        let next = html[i..].find('<').map(|n| i + n).unwrap_or(html.len());
        out.push_str(&html[i..next]);
        i = next;
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let mut clean = String::new();
    let mut blank = 0;
    for line in out.lines().map(str::trim) {
        if line.is_empty() {
            blank += 1;
            if blank == 1 {
                clean.push('\n');
            }
        } else {
            blank = 0;
            clean.push_str(line);
            clean.push('\n');
        }
    }
    clean.trim().to_string()
}

fn ws_cmd(rag: &mut Rag, cmd: WsCommand) -> Result<()> {
    match cmd {
        WsCommand::List { json } => {
            let rows = rag.ws_list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            println!("/");
            let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
            for (i, r) in rows.iter().enumerate() {
                let depth = r.path.matches('/').count();
                // Tree guides: for each ancestor level, is there a later sibling?
                let mut prefix = String::new();
                for d in 0..depth {
                    let anc: String = r.path.split('/').take(d + 1).collect::<Vec<_>>().join("/");
                    prefix.push_str(if has_later_sibling(&paths, i, &anc) {
                        "│   "
                    } else {
                        "    "
                    });
                }
                let last = !has_later_sibling(&paths, i, &r.path);
                let counts = [(r.docs - r.notes, "doc"), (r.notes, "note")]
                    .iter()
                    .filter(|(n, _)| *n > 0)
                    .map(|(n, what)| format!("{n} {what}{}", if *n == 1 { "" } else { "s" }))
                    .collect::<Vec<_>>()
                    .join(", ");
                let counts = if counts.is_empty() {
                    counts
                } else {
                    format!("  ({counts})")
                };
                println!(
                    "{prefix}{}{}{counts}",
                    if last { "└── " } else { "├── " },
                    ws::leaf(&r.path)
                );
            }
            Ok(())
        }
        WsCommand::Create { path, force } => {
            let (_, created) = rag.ws_create(&path, force)?;
            let norm = ws::normalize(&path)?.unwrap_or_default();
            println!("{} {norm}", if created { "created" } else { "exists" });
            Ok(())
        }
        WsCommand::Rm { path, force } => {
            let n = rag.ws_rm(&path, force)?;
            println!(
                "removed {} ({n} documents)",
                ws::normalize(&path)?.unwrap_or_default()
            );
            Ok(())
        }
    }
}

/// Whether a workspace after index `i` shares `path`'s parent (so `path`
/// is not the last child drawn at its level).
fn has_later_sibling(paths: &[&str], i: usize, path: &str) -> bool {
    let parent = ws::parent(path);
    paths[i + 1..].iter().any(|p| ws::parent(p) == parent)
}

/// `3d ago`, or `just now`.
fn ago(t: i64) -> String {
    match age(t).as_str() {
        "now" => "just now".into(),
        a => format!("{a} ago"),
    }
}

fn note_id(s: &str) -> Result<i64> {
    s.trim_start_matches("note:")
        .parse()
        .map_err(|_| anyhow!("bad note id {s:?} (note:<n> or <n>)"))
}

fn note_cmd(rag: &mut Rag, ctx: &Ctx, cmd: NoteCommand) -> Result<()> {
    match cmd {
        NoteCommand::List { ws, deleted, json } => {
            let paths = ctx.scope(ws);
            let scope = resolve_scope(rag, &paths)?;
            let notes = rag.notes(scope.as_deref(), deleted)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&notes)?);
                return Ok(());
            }
            println!("scope: {}", paths.join(", "));
            if notes.is_empty() {
                println!("no notes");
            }
            for n in notes {
                println!(
                    "note:{:<5} {:<24} {:>4}  {}",
                    n.id,
                    n.ws,
                    age(n.updated),
                    n.title
                );
            }
            Ok(())
        }
        NoteCommand::Show { id, json } => {
            let n = rag.note(note_id(&id)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&n)?);
                return Ok(());
            }
            println!("note:{}  {}", n.id, n.title);
            println!(
                "ws: {}  updated: {}  created: {}  source: {}{}",
                n.ws,
                ago(n.updated),
                ago(n.created),
                n.source.as_deref().unwrap_or("-"),
                n.deleted_at
                    .map(|d| format!("  DELETED {}", ago(d)))
                    .unwrap_or_default()
            );
            println!("\n{}", n.body);
            Ok(())
        }
        NoteCommand::Edit { id, text, title } => {
            let id = note_id(&id)?;
            // --title alone keeps the text; no flags: stdin when piped (the
            // ssh hop), else $EDITOR.
            let text = match (text, &title) {
                (Some(t), _) => Some(t),
                (None, Some(_)) => None,
                (None, None) if !std::io::stdin().is_terminal() => Some(read_stdin()?),
                (None, None) => Some(edit_in_editor(&rag.note(id)?.body)?),
            };
            rag.note_edit(id, title.as_deref(), text.as_deref(), ctx.source.as_deref())?;
            println!("updated note:{id}");
            Ok(())
        }
        NoteCommand::Rm { id } => {
            let id = note_id(&id)?;
            rag.note_rm(id)?;
            println!("deleted note:{id} (purged in 30 days)");
            Ok(())
        }
        NoteCommand::Mv { id, ws } => {
            let id = note_id(&id)?;
            let to = rag.note_mv(id, &ws)?;
            println!("moved note:{id} → {to}");
            Ok(())
        }
    }
}

fn obs_id(s: &str) -> Result<i64> {
    s.strip_prefix("obs:")
        .or_else(|| s.strip_prefix("note:"))
        .unwrap_or(s)
        .parse()
        .map_err(|_| anyhow!("bad observation id {s:?} (obs:<n>, note:<n> or <n>)"))
}

fn obs_cmd(rag: &mut Rag, ctx: &Ctx, cmd: ObsCommand) -> Result<()> {
    match cmd {
        ObsCommand::Add {
            text,
            category,
            title,
            app,
        } => {
            let o = rag.obs_add(
                app.as_deref().or(ctx.app.as_deref()),
                category,
                title.as_deref(),
                &text.join(" "),
                ctx.source.as_deref(),
            )?;
            println!("saved obs:{} → {}", o.id, o.ws);
            Ok(())
        }
        ObsCommand::List {
            status,
            category,
            app,
            since,
            json,
        } => {
            let all = rag.obs_list(&ObsFilter {
                status: status.0,
                category,
                app,
                since,
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
                return Ok(());
            }
            if all.is_empty() {
                println!("no observations");
            }
            let mixed = all.iter().any(|o| o.status != all[0].status);
            for o in &all {
                let status = if mixed {
                    format!("{:<10}", o.status.as_str())
                } else {
                    String::new()
                };
                println!(
                    "obs:{:<5} {:>4}  {:<14} {:<11}{status} {}",
                    o.id,
                    age(o.created),
                    o.app,
                    o.category.as_str(),
                    o.title
                );
            }
            Ok(())
        }
        ObsCommand::Show { id, json } => {
            let o = rag.obs(obs_id(&id)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&o)?);
                return Ok(());
            }
            print_obs(&o);
            Ok(())
        }
        ObsCommand::Done { ids, note } => obs_apply(rag, &ids, Transition::Done(note.as_deref())),
        ObsCommand::Dismiss { ids, note } => {
            obs_apply(rag, &ids, Transition::Dismiss(note.as_deref()))
        }
        ObsCommand::Reopen { ids } => obs_apply(rag, &ids, Transition::Reopen),
        ObsCommand::Stats { since, json } => {
            let stats = rag.obs_stats(since)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
                return Ok(());
            }
            println!("{} observations", stats.total);
            for (label, counts) in [
                ("status", &stats.by_status),
                ("category", &stats.by_category),
                ("app", &stats.by_app),
            ] {
                let mut rows: Vec<_> = counts.iter().collect();
                rows.sort_by(|a, b| b.1.cmp(a.1));
                let rows: Vec<String> = rows.iter().map(|(k, n)| format!("{k} {n}")).collect();
                println!("{label:<9} {}", rows.join(", "));
            }
            Ok(())
        }
    }
}

fn print_obs(o: &Obs) {
    println!(
        "obs:{}  {} · {}  {}",
        o.id,
        o.category.as_str(),
        o.status.as_str(),
        o.title
    );
    println!(
        "app: {}  ws: {}  created: {}  source: {}",
        o.app,
        o.ws,
        ago(o.created),
        o.source.as_deref().unwrap_or("-")
    );
    if let Some(t) = o.closed_at {
        println!(
            "closed: {}  resolution: {}",
            ago(t),
            o.resolution.as_deref().unwrap_or("-")
        );
    }
    println!("\n{}", o.body);
}

fn obs_apply(rag: &Rag, ids: &[String], transition: Transition) -> Result<()> {
    let mut failed = 0;
    for id in ids {
        match obs_id(id).and_then(|n| rag.obs_apply(n, transition)) {
            Ok(o) => println!("obs:{} {}", o.id, o.status.as_str()),
            Err(e) => {
                failed += 1;
                eprintln!("nebula rag: {e}");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} not changed", ids.len());
    }
    Ok(())
}

fn edit_in_editor(body: &str) -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!(
        "nebula-rag-note-{}-{}.md",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    // create_new: never follow a file or symlink someone planted there.
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        f.write_all(body.as_bytes())?;
    }
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .context("run $EDITOR")?;
    let text = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    if !status.success() {
        bail!("editor exited with {status}; note unchanged");
    }
    Ok(text?)
}

fn app_cmd(rag: &Rag, cmd: AppCommand) -> Result<()> {
    match cmd {
        AppCommand::Add {
            name,
            repo,
            branch,
            force,
        } => {
            let app = rag.app_add(&name, &repo, branch.as_deref(), force)?;
            println!(
                "app {} → {} (origin/{}, remote {})\nnext: nebula rag sync {}",
                app.name,
                app.repo,
                app.branch,
                app.remote.as_deref().unwrap_or("none"),
                app.name
            );
            Ok(())
        }
        AppCommand::List { json } => {
            let apps = rag.apps()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&apps)?);
                return Ok(());
            }
            if apps.is_empty() {
                println!("no apps (nebula rag app add <name> --repo <path>)");
            }
            for a in apps {
                println!(
                    "{:<16} {}  origin/{}  {}",
                    a.name,
                    a.repo,
                    a.branch,
                    match (&a.last_commit, a.synced_at) {
                        (Some(c), Some(t)) =>
                            format!("synced {} ({})", &c[..c.len().min(10)], ago(t)),
                        _ => "never synced".into(),
                    }
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_keeps_headings_and_text_and_drops_scripts() {
        let html = "<html><head><style>p{}</style><script>var x=1;</script></head><body>\
                    <h1>Title</h1><p>Hello &amp; welcome</p><h2 class=x>Sub</h2><div>body</div></body></html>";
        let t = html_to_text(html);
        assert!(t.contains("# Title"), "{t}");
        assert!(t.contains("## Sub"), "{t}");
        assert!(t.contains("Hello & welcome"));
        assert!(!t.contains("var x"));
        assert!(!t.contains("p{}"));
    }

    #[test]
    fn html_with_a_trailing_bracket_or_non_ascii_does_not_panic() {
        assert_eq!(html_to_text("a <"), "a");
        let t = html_to_text("<p>İstanbul și <b>București</b></p><h2>Ține</h2>x");
        assert!(t.contains("İstanbul și București"), "{t}");
        assert!(t.contains("## Ține"), "{t}");
    }

    #[test]
    fn globals_are_stripped_only_before_the_subcommand() {
        let mut a: Vec<String> = ["--source", "x", "--origin=u", "add", "--source", "text"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        strip_globals(&mut a);
        assert_eq!(a, vec!["add", "--source", "text"]);
    }

    #[test]
    fn an_observation_id_reads_with_either_label_or_bare() {
        assert_eq!(obs_id("obs:7").unwrap(), 7);
        assert_eq!(obs_id("note:7").unwrap(), 7);
        assert_eq!(obs_id("7").unwrap(), 7);
        assert_eq!(
            obs_id("seven").unwrap_err().to_string(),
            "bad observation id \"seven\" (obs:<n>, note:<n> or <n>)"
        );
    }

    #[test]
    fn one_id_that_is_not_an_observation_fails_the_command_after_the_rest() {
        let mut rag = Rag::with_conn(
            crate::db::open_memory().unwrap(),
            Box::new(crate::embed::fake::FakeEmbedder::default()),
        );
        let a = rag
            .obs_add(None, Category::Tool, None, "first failure", None)
            .unwrap()
            .id;
        let b = rag
            .obs_add(None, Category::Tool, None, "second failure", None)
            .unwrap()
            .id;
        let ids = [a.to_string(), "999".to_string(), format!("note:{b}")];
        let err = obs_apply(&rag, &ids, Transition::Done(None)).unwrap_err();
        assert_eq!(err.to_string(), "1 of 3 not changed");
        assert_eq!(rag.obs(a).unwrap().status, Status::Done);
        assert_eq!(rag.obs(b).unwrap().status, Status::Done);
    }

    #[test]
    fn default_scope_is_the_app_and_shared() {
        let ctx = Ctx {
            app: Some("nebula".into()),
            source: None,
        };
        assert_eq!(ctx.scope(vec![]), vec!["apps/nebula", "shared"]);
        assert_eq!(ctx.scope(vec!["/".into()]), vec!["/"]);
        let none = Ctx {
            app: None,
            source: None,
        };
        assert_eq!(none.scope(vec![]), vec!["/"]);
    }
}

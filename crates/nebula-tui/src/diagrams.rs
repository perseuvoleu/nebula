//! Diagrams: one self-contained `<slug>.html` per diagram, all of them in
//! `<checkout>/.scratch/diagrams/`. The CLI (`nebula diagram …`) and the ⌘X
//! viewer both go through this module and never look anywhere else.
//!
//! There is no index file: title, tags and the owning session are read from
//! the head of each file (`<title>` and `<meta name="nebula-diagram">`), and
//! a listing is a folder scan sorted newest first.

use nebula_core::{Agent, Project, Worktree};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

/// The one folder, relative to a checkout's root.
pub const DIR: &str = ".scratch/diagrams";
/// How much of a file is read for its title and meta.
const HEAD_BYTES: usize = 8 * 1024;
const MAX_SLUG: usize = 64;

pub fn dir(checkout: &Path) -> PathBuf {
    checkout.join(DIR)
}

pub fn file(checkout: &Path, slug: &str) -> PathBuf {
    dir(checkout).join(format!("{slug}.html"))
}

pub fn decisions_file(checkout: &Path, slug: &str) -> PathBuf {
    dir(checkout).join(format!("{slug}.decisions.jsonl"))
}

/// The checkout owning `from`: the git toplevel (a worktree's own root, not
/// the primary's), or `from` itself outside any repository.
pub fn checkout_root(from: &Path) -> PathBuf {
    std::process::Command::new("git")
        .arg("-C")
        .arg(from)
        .args(["rev-parse", "--show-toplevel"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| from.to_path_buf())
}

/// `[a-z0-9-]+`, bounded so a slug is always a sane file name.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_SLUG
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Best-effort slug from a file stem ("Auth Flow_v2" → "auth-flow-v2").
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(MAX_SLUG);
    slug.trim_end_matches('-').to_owned()
}

/// What `<meta name="nebula-diagram">` carries. Everything is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub tags: Vec<String>,
    /// Owning session's name when the diagram was created (names change).
    pub session: Option<String>,
    /// Owning session's stable id — what ownership is decided on first.
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    pub slug: String,
    pub path: PathBuf,
    /// `<title>`, or the slug when the file has none.
    pub title: String,
    pub meta: Meta,
    pub mtime: SystemTime,
}

impl Diagram {
    /// Who made it, for "same session" comparisons: the id when recorded,
    /// else the name.
    pub fn owner_key(&self) -> Option<&str> {
        self.meta.agent.as_deref().or(self.meta.session.as_deref())
    }
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(needle)
}

/// `<title>` text, whitespace-collapsed; None when absent or empty.
pub fn parse_title(head: &str) -> Option<String> {
    let open = find_ci(head, "<title")?;
    let start = open + head[open..].find('>')? + 1;
    let end = start + find_ci(&head[start..], "</title")?;
    let title = unescape(&head[start..end])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!title.is_empty()).then_some(title)
}

/// The nebula meta tag's JSON. Missing, malformed or wrongly-typed meta is
/// simply empty — a diagram without it still lists and renders.
pub fn parse_meta(head: &str) -> Meta {
    let parsed = (|| {
        let lower = head.to_ascii_lowercase();
        let name = lower
            .find("name=\"nebula-diagram\"")
            .or_else(|| lower.find("name='nebula-diagram'"))?;
        let tag_start = lower[..name].rfind('<')?;
        let tag = &head[tag_start..name + head[name..].find('>')?];
        let content = find_ci(tag, "content=")? + "content=".len();
        let quote = tag[content..].chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let body = &tag[content + 1..];
        let body = &body[..body.find(quote)?];
        serde_json::from_str::<Value>(&unescape(body)).ok()
    })();
    let Some(value) = parsed else {
        return Meta::default();
    };
    let text = |key: &str| {
        value[key]
            .as_str()
            .map(|s| clean(s, 200))
            .filter(|s| !s.is_empty())
    };
    Meta {
        tags: value["tags"]
            .as_array()
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(|t| clean(t, 40))
                    .filter(|t| !t.is_empty())
                    .take(16)
                    .collect()
            })
            .unwrap_or_default(),
        session: text("session"),
        agent: text("agent"),
    }
}

fn read_head(path: &Path) -> String {
    let mut head = Vec::with_capacity(HEAD_BYTES);
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(HEAD_BYTES as u64).read_to_end(&mut head);
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// Every diagram of `checkout`, newest first (slug breaks mtime ties).
pub fn scan(checkout: &Path) -> Vec<Diagram> {
    scan_reusing(checkout, &[])
}

/// [`scan`] for the viewer's folder watch: a file whose mtime did not move
/// keeps its `previous` entry, so a quiet poll is one `read_dir` plus stats.
pub fn scan_reusing(checkout: &Path, previous: &[Diagram]) -> Vec<Diagram> {
    let Ok(entries) = std::fs::read_dir(dir(checkout)) else {
        return Vec::new();
    };
    let mut diagrams: Vec<Diagram> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let slug = path.file_name()?.to_str()?.strip_suffix(".html")?.to_owned();
            if !valid_slug(&slug) {
                return None;
            }
            let info = entry.metadata().ok().filter(|m| m.is_file())?;
            let mtime = info.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if let Some(same) = previous.iter().find(|d| d.slug == slug && d.mtime == mtime) {
                return Some(same.clone());
            }
            let head = read_head(&path);
            Some(Diagram {
                title: parse_title(&head).unwrap_or_else(|| slug.clone()),
                meta: parse_meta(&head),
                slug,
                path,
                mtime,
            })
        })
        .collect();
    diagrams.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.slug.cmp(&b.slug)));
    diagrams
}

/// Indices of the diagrams matching `query`, best first; fuzzy on slug,
/// title and tags. An empty query keeps everything in listing order.
pub fn search(query: &str, diagrams: &[Diagram]) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..diagrams.len()).collect();
    }
    let mut hits: Vec<(i32, usize)> = diagrams
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            std::iter::once(d.slug.as_str())
                .chain(std::iter::once(d.title.as_str()))
                .chain(d.meta.tags.iter().map(String::as_str))
                .filter_map(|field| crate::fuzzy::fuzzy_match(query, field))
                .map(|m| m.score)
                .max()
                .map(|score| (score, i))
        })
        .collect();
    // Stable on the listing order, so equal scores stay newest first.
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Problems `nebula diagram add` reports: `Err` refuses the file, the `Ok`
/// lines are warnings.
pub fn validate(html: &str) -> Result<Vec<String>, String> {
    let lower = html.to_ascii_lowercase();
    if !lower.contains("<html") && !lower.contains("<!doctype html") {
        return Err("not an HTML document (no <html> or <!doctype html>)".into());
    }
    if parse_title(html).is_none() {
        return Err("no <title> — the viewer lists diagrams by title".into());
    }
    let mut warnings = Vec::new();
    for tag in lower.split('<').skip(1) {
        let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
        let loads = (tag.starts_with("script") && tag.contains("src="))
            || (tag.starts_with("link") && tag.contains("href="));
        if loads && (tag.contains("http://") || tag.contains("https://") || tag.contains("=\"//")) {
            warnings.push(format!(
                "network reference, blocked in the viewer: <{}>",
                tag.chars().take(120).collect::<String>()
            ));
        }
    }
    Ok(warnings)
}

/// Which page `nebula diagram new` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Plain,
    Decision,
}

const TEMPLATE_PLAIN: &str = include_str!("../assets/diagram-template.html");
const TEMPLATE_DECISION: &str = include_str!("../assets/diagram-decision-template.html");

/// The template with title and meta filled in.
pub fn render_template(kind: Template, slug: &str, title: &str, meta: &Meta) -> String {
    let mut data = json!({ "tags": meta.tags });
    if let Some(session) = &meta.session {
        data["session"] = json!(session);
    }
    if let Some(agent) = &meta.agent {
        data["agent"] = json!(agent);
    }
    match kind {
        Template::Plain => TEMPLATE_PLAIN,
        Template::Decision => TEMPLATE_DECISION,
    }
    .replace("{{TITLE}}", &escape(title))
    // Single-quoted attribute: the JSON's own double quotes stay readable.
    .replace(
        "{{META}}",
        &data
            .to_string()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\'', "&#39;"),
    )
    .replace("{{SLUG}}", slug)
}

/// Strip control characters and cap the length (in chars).
pub fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// A choice made on a decision page. Only these capped strings ever leave
/// the page; nothing else it says reaches the TUI or a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: String,
    pub choice: String,
    pub label: String,
    pub note: Option<String>,
}

impl Decision {
    /// From the worker's `decision` line. Non-string fields count as absent;
    /// a decision without an id or a choice is dropped.
    pub fn from_value(value: &Value) -> Option<Self> {
        let field = |key: &str, max: usize| {
            clean(value[key].as_str().unwrap_or_default(), max)
                .trim()
                .to_owned()
        };
        let decision = Self {
            id: field("id", 64),
            choice: field("choice", 64),
            label: field("label", 200),
            note: Some(field("note", 2000)).filter(|n| !n.is_empty()),
        };
        (!decision.id.is_empty() && !decision.choice.is_empty()).then_some(decision)
    }

    /// The one line typed into the owning session.
    pub fn prompt(&self, slug: &str, decisions: &Path) -> String {
        let note = self
            .note
            .as_ref()
            .map(|n| format!(" — note: {n}"))
            .unwrap_or_default();
        format!(
            "[nebula diagram] {slug}: decision \"{}\" = \"{}\" ({}){note}. Decisions file: {}",
            self.id,
            self.choice,
            self.label,
            decisions.display()
        )
    }
}

/// Append one decision to `<slug>.decisions.jsonl`; returns the file.
pub fn append_decision(
    checkout: &Path,
    slug: &str,
    decision: &Decision,
    ts_ms: i64,
) -> std::io::Result<PathBuf> {
    let path = decisions_file(checkout, slug);
    std::fs::create_dir_all(dir(checkout))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = json!({
        "ts": ts_ms,
        "slug": slug,
        "id": decision.id,
        "choice": decision.choice,
        "label": decision.label,
        "note": decision.note,
    });
    writeln!(file, "{line}")?;
    Ok(path)
}

/// Every recorded decision of `slug`, oldest first; bad lines are skipped.
pub fn read_decisions(checkout: &Path, slug: &str) -> Vec<Value> {
    std::fs::read_to_string(decisions_file(checkout, slug))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canonical(a) == canonical(b)
}

/// The session a decision on `diagram` goes to: the one recorded in its
/// meta, and only while that is a live local session of this very checkout.
/// Anything else — no meta, dead or archived, another checkout, a remote
/// twin, an ambiguous name — is None, and the decision is only saved.
pub fn resolve_owner<'a>(
    diagram: &Diagram,
    checkout: &Path,
    projects: &[Project],
    worktrees: &[Worktree],
    agents: &'a [Agent],
) -> Option<&'a Agent> {
    let here = |agent: &&Agent| {
        agent.alive
            && !agent.archived
            && worktrees.iter().any(|w| {
                w.id == agent.worktree_id
                    && same_dir(&w.path, checkout)
                    && projects
                        .iter()
                        .any(|p| p.id == w.project_id && p.host.is_none())
            })
    };
    if let Some(id) = &diagram.meta.agent {
        return agents.iter().filter(here).find(|a| a.id.as_str() == id);
    }
    let name = diagram.meta.session.as_ref()?;
    let mut named = agents.iter().filter(here).filter(|a| &a.name == name);
    let first = named.next()?;
    named.next().is_none().then_some(first)
}

/// Follow-up pages: while the viewer shows `current`, a different diagram by
/// the same session that just appeared (or changed) and is newer than the
/// shown one takes the focus. Returns its index in `after`.
pub fn auto_focus(current: &Diagram, before: &[Diagram], after: &[Diagram]) -> Option<usize> {
    let owner = current.owner_key()?;
    let shown = after
        .iter()
        .find(|d| d.slug == current.slug)
        .map_or(current.mtime, |d| d.mtime);
    // `after` is newest first, so the first hit is the newest follow-up.
    after.iter().position(|d| {
        d.slug != current.slug
            && d.owner_key() == Some(owner)
            && d.mtime > shown
            && !before.iter().any(|old| old.slug == d.slug && old.mtime == d.mtime)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nebula_core::{AgentId, AgentKind, AgentStatus, ProjectId, WorktreeId};
    use std::time::Duration;

    fn write(checkout: &Path, slug: &str, html: &str, age_secs: u64) -> PathBuf {
        std::fs::create_dir_all(dir(checkout)).unwrap();
        let path = file(checkout, slug);
        std::fs::write(&path, html).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
            .unwrap();
        path
    }

    fn page(title: &str, meta: &str) -> String {
        format!(
            "<!doctype html><html><head><title>{title}</title>\
             <meta name=\"nebula-diagram\" content='{meta}'></head><body></body></html>"
        )
    }

    #[test]
    fn slugs_are_lowercase_alphanumerics_and_hyphens() {
        for ok in ["a", "auth-flow", "v2", "0-1-2"] {
            assert!(valid_slug(ok), "{ok}");
        }
        for bad in ["", "Auth", "a b", "a_b", "a.html", "../x", "a/b", "é"] {
            assert!(!valid_slug(bad), "{bad}");
        }
        assert!(!valid_slug(&"a".repeat(65)));
        assert_eq!(slugify("Auth Flow_v2.final"), "auth-flow-v2-final");
        assert_eq!(slugify("  --x--  "), "x");
    }

    #[test]
    fn title_and_meta_come_from_the_head() {
        let head = page(
            "  Auth &amp; session\n flow ",
            r#"{"tags":["auth","flow"],"session":"worker one","agent":"01ABC"}"#,
        );
        assert_eq!(parse_title(&head).as_deref(), Some("Auth & session flow"));
        assert_eq!(
            parse_meta(&head),
            Meta {
                tags: vec!["auth".into(), "flow".into()],
                session: Some("worker one".into()),
                agent: Some("01ABC".into()),
            }
        );
        // Double-quoted attribute with entity-encoded JSON, attributes swapped.
        let swapped = "<META content=\"{&quot;tags&quot;:[&quot;x&quot;]}\" name='nebula-diagram'>";
        assert_eq!(parse_meta(swapped).tags, ["x"]);
    }

    #[test]
    fn missing_or_invalid_meta_is_empty_not_an_error() {
        assert_eq!(parse_meta("<html><title>t</title>"), Meta::default());
        assert_eq!(parse_meta(&page("t", "{not json")), Meta::default());
        assert_eq!(parse_meta(&page("t", r#"{"tags":"nope","session":7}"#)), Meta::default());
        assert_eq!(parse_title("<html><title>  </title>"), None);
        assert_eq!(parse_title("<html>"), None);
        // Control characters never survive into the listing.
        assert_eq!(
            parse_meta(&page("t", r#"{"session":"a\u001b[31mb"}"#)).session.as_deref(),
            Some("a[31mb")
        );
    }

    #[test]
    fn scan_lists_only_the_folder_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "old", &page("Old", "{}"), 300);
        write(tmp.path(), "new", &page("New", r#"{"tags":["x"]}"#), 10);
        write(tmp.path(), "mid", "<html>no title</html>", 100);
        // Not diagrams: wrong extension, invalid slug, a decisions log, a
        // file outside the folder.
        std::fs::write(dir(tmp.path()).join("notes.md"), "x").unwrap();
        std::fs::write(dir(tmp.path()).join("Bad Name.html"), "x").unwrap();
        std::fs::write(decisions_file(tmp.path(), "new"), "{}\n").unwrap();
        std::fs::write(tmp.path().join(".scratch/stray.html"), page("Stray", "{}")).unwrap();

        let found = scan(tmp.path());
        let slugs: Vec<_> = found.iter().map(|d| d.slug.as_str()).collect();
        assert_eq!(slugs, ["new", "mid", "old"]);
        assert_eq!(found[0].title, "New");
        assert_eq!(found[1].title, "mid", "no <title> falls back to the slug");
        assert!(scan(&tmp.path().join("nowhere")).is_empty());
    }

    #[test]
    fn a_rescan_reuses_unchanged_entries_and_rereads_changed_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), "a", &page("First", "{}"), 50);
        let mut before = scan(tmp.path());
        // Same mtime: the cached entry wins even though the parse would differ.
        before[0].title = "Cached".into();
        assert_eq!(scan_reusing(tmp.path(), &before)[0].title, "Cached");
        std::fs::write(&path, page("Second", "{}")).unwrap();
        assert_eq!(scan_reusing(tmp.path(), &before)[0].title, "Second");
    }

    #[test]
    fn search_is_fuzzy_over_slug_title_and_tags() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "auth-flow", &page("Login sequence", "{}"), 10);
        write(tmp.path(), "db", &page("Storage layout", r#"{"tags":["postgres"]}"#), 20);
        write(tmp.path(), "deploy", &page("Release pipeline", "{}"), 30);
        let all = scan(tmp.path());
        let slugs = |q: &str| -> Vec<&str> {
            search(q, &all).into_iter().map(|i| all[i].slug.as_str()).collect()
        };
        assert_eq!(slugs(""), ["auth-flow", "db", "deploy"]);
        assert_eq!(slugs("auth"), ["auth-flow"]);
        assert_eq!(slugs("STOR"), ["db"], "title, case-insensitive");
        assert_eq!(slugs("pgres"), ["db"], "tags, subsequence");
        assert!(slugs("zzz").is_empty());
    }

    #[test]
    fn add_validation_wants_html_with_a_title_and_warns_on_network() {
        assert!(validate("just text").is_err());
        assert!(validate("<html><body>x</body></html>").is_err());
        assert_eq!(validate(&page("T", "{}")), Ok(vec![]));
        let remote = "<html><title>T</title>\
            <script src=\"https://cdn.example/m.js\"></script>\
            <link rel=stylesheet href='http://x/y.css'>\
            <a href=\"https://fine.example\">a link is fine</a>";
        assert_eq!(validate(remote).unwrap().len(), 2);
    }

    #[test]
    fn templates_round_trip_through_the_parser_and_stay_offline() {
        let meta = Meta {
            tags: vec!["it's".into(), "a<b".into()],
            session: Some("fix \"login\"".into()),
            agent: Some("01XYZ".into()),
        };
        for kind in [Template::Plain, Template::Decision] {
            let html = render_template(kind, "my-slug", "Tom & <Jerry>", &meta);
            assert_eq!(parse_title(&html).as_deref(), Some("Tom & <Jerry>"));
            assert_eq!(parse_meta(&html[..HEAD_BYTES.min(html.len())]), meta);
            assert_eq!(validate(&html), Ok(vec![]), "no network references");
            assert!(!html.contains("{{"), "every placeholder filled");
        }
        let decision = render_template(Template::Decision, "s", "T", &Meta::default());
        assert!(decision.contains("window.nebula.decide("));
    }

    #[test]
    fn decision_fields_are_capped_and_stripped() {
        let raw = json!({
            "id": "i".repeat(100),
            "choice": " b\u{1b}[2J\r\n ",
            "label": "l".repeat(300),
            "note": "n".repeat(3000),
            "extra": "never forwarded",
        });
        let d = Decision::from_value(&raw).unwrap();
        assert_eq!(d.id.len(), 64);
        assert_eq!(d.choice, "b[2J");
        assert_eq!(d.label.len(), 200);
        assert_eq!(d.note.as_ref().unwrap().len(), 2000);
        // Wrong types and empty essentials are dropped, not coerced.
        assert_eq!(Decision::from_value(&json!({"id": 1, "choice": "a"})), None);
        assert_eq!(Decision::from_value(&json!({"id": "x", "choice": "\n"})), None);
        assert_eq!(Decision::from_value(&json!("text")), None);
    }

    #[test]
    fn the_prompt_is_one_fixed_format_line() {
        let mut d = Decision::from_value(
            &json!({"id":"db","choice":"pg","label":"Postgres","note":"line one\nline two"}),
        )
        .unwrap();
        let path = Path::new("/w/.scratch/diagrams/pick.decisions.jsonl");
        assert_eq!(
            d.prompt("pick", path),
            "[nebula diagram] pick: decision \"db\" = \"pg\" (Postgres) — note: line oneline two. \
             Decisions file: /w/.scratch/diagrams/pick.decisions.jsonl"
        );
        d.note = None;
        let line = d.prompt("pick", path);
        assert!(!line.contains("note:") && !line.contains('\n'));
    }

    #[test]
    fn decisions_append_to_a_jsonl_next_to_the_diagram() {
        let tmp = tempfile::tempdir().unwrap();
        let d = |choice: &str| {
            Decision::from_value(&json!({"id":"q","choice":choice,"label":"L"})).unwrap()
        };
        let path = append_decision(tmp.path(), "pick", &d("a"), 1).unwrap();
        append_decision(tmp.path(), "pick", &d("b"), 2).unwrap();
        assert_eq!(path, decisions_file(tmp.path(), "pick"));
        let rows = read_decisions(tmp.path(), "pick");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], json!({"ts":1,"slug":"pick","id":"q","choice":"a","label":"L","note":null}));
        assert_eq!(rows[1]["choice"], "b");
        assert!(read_decisions(tmp.path(), "other").is_empty());
        assert!(scan(tmp.path()).is_empty(), "a decisions log is not a diagram");
    }

    fn project(id: &str, host: Option<&str>) -> Project {
        Project {
            id: ProjectId(id.into()),
            name: id.into(),
            workspace_id: Default::default(),
            repo_path: PathBuf::from("/repo"),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: host.map(str::to_owned),
        }
    }

    fn worktree(id: &str, project: &str, path: &Path) -> Worktree {
        Worktree {
            id: WorktreeId(id.into()),
            project_id: ProjectId(project.into()),
            path: path.to_path_buf(),
            branch: id.into(),
            is_main: false,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        }
    }

    fn agent(id: &str, name: &str, worktree: &str, alive: bool) -> Agent {
        Agent {
            id: AgentId(id.into()),
            worktree_id: WorktreeId(worktree.into()),
            name: name.into(),
            status: AgentStatus::Finished,
            archived: false,
            archived_at: 0,
            pinned: false,
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            session_id: None,
            sort_order: 0,
            status_changed_at: 0,
            alive,
            branch: None,
        }
    }

    fn owned(slug: &str, session: Option<&str>, agent: Option<&str>, age_secs: u64) -> Diagram {
        Diagram {
            slug: slug.into(),
            path: PathBuf::from(format!("/x/{slug}.html")),
            title: slug.into(),
            meta: Meta {
                tags: vec![],
                session: session.map(str::to_owned),
                agent: agent.map(str::to_owned),
            },
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
        }
    }

    #[test]
    fn a_decision_goes_only_to_a_live_owner_on_the_same_checkout() {
        let here = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let projects = [project("p", None), project("r", Some("findl"))];
        let worktrees = [
            worktree("w-here", "p", here.path()),
            worktree("w-else", "p", elsewhere.path()),
            // A remote twin whose path happens to equal the local one.
            worktree("w-remote", "r", here.path()),
        ];
        let mut archived = agent("a-arch", "archived", "w-here", true);
        archived.archived = true;
        let agents = [
            agent("a-live", "owner", "w-here", true),
            agent("a-dead", "sleeper", "w-here", false),
            agent("a-else", "neighbour", "w-else", true),
            agent("a-remote", "twin", "w-remote", true),
            archived,
            agent("a-dup1", "dup", "w-here", true),
            agent("a-dup2", "dup", "w-here", true),
        ];
        let owner = |session: Option<&str>, id: Option<&str>| {
            resolve_owner(&owned("d", session, id, 0), here.path(), &projects, &worktrees, &agents)
                .map(|a| a.id.as_str().to_owned())
        };
        assert_eq!(owner(Some("owner"), None).as_deref(), Some("a-live"));
        assert_eq!(owner(None, Some("a-live")).as_deref(), Some("a-live"));
        // The id decides: a stale name next to a live id still resolves, and
        // a live name never rescues an id that is not deliverable.
        assert_eq!(owner(Some("renamed since"), Some("a-live")).as_deref(), Some("a-live"));
        assert_eq!(owner(Some("owner"), Some("a-dead")), None);
        assert_eq!(owner(None, None), None, "no owner recorded");
        assert_eq!(owner(Some("ghost"), None), None, "unknown session");
        assert_eq!(owner(Some("sleeper"), None), None, "dead session");
        assert_eq!(owner(Some("archived"), None), None, "archived session");
        assert_eq!(owner(Some("neighbour"), None), None, "another checkout");
        assert_eq!(owner(None, Some("a-else")), None, "another checkout, by id");
        assert_eq!(owner(Some("twin"), None), None, "remote twin");
        assert_eq!(owner(Some("dup"), None), None, "ambiguous name");
    }

    #[test]
    fn a_newer_page_by_the_same_session_takes_the_focus() {
        let shown = owned("ask", None, Some("s1"), 100);
        let before = vec![shown.clone(), owned("other", None, Some("s2"), 200)];

        // Same session, new and newer: focus moves.
        let after = vec![owned("answer", None, Some("s1"), 10), before[0].clone(), before[1].clone()];
        assert_eq!(auto_focus(&shown, &before, &after), Some(0));

        // Another session's new page, or an unowned one: stay put.
        let after = vec![owned("noise", None, Some("s2"), 10), owned("anon", None, None, 5), shown.clone()];
        assert_eq!(auto_focus(&shown, &before, &after), None);

        // Nothing changed since the last scan: stay put (the user may have
        // switched back to an older page on purpose).
        let settled = vec![owned("answer", None, Some("s1"), 10), shown.clone()];
        assert_eq!(auto_focus(&shown, &settled, &settled), None);

        // The shown page itself was rewritten more recently: it reloads
        // instead, the older sibling does not steal the focus.
        let after = vec![owned("ask", None, Some("s1"), 1), owned("answer", None, Some("s1"), 10)];
        assert_eq!(auto_focus(&shown, &before, &after), None);

        // An unowned page never auto-focuses anything.
        let anon = owned("anon", None, None, 100);
        assert_eq!(auto_focus(&anon, &[], &[owned("x", None, None, 1)]), None);

        // Names work as the owner key when no id was recorded.
        let named = owned("ask", Some("worker"), None, 100);
        let after = vec![owned("next", Some("worker"), None, 1), named.clone()];
        assert_eq!(auto_focus(&named, &[named.clone()], &after), Some(0));
    }
}

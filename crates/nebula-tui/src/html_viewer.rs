//! The ⌘X diagram viewer: a live HTML document inside the outer Ghostty TUI.
//! Not a native WebView: a headless Chromium (Node + Playwright worker)
//! produces frames, Kitty graphics displays them, and terminal input is
//! forwarded to the live DOM. No browser window, no daemon/PTY involvement.
//!
//! It shows the diagrams of one checkout — `.scratch/diagrams/`, through
//! [`crate::diagrams`], nothing else — newest first, with a list strip to
//! switch between them, `/` to search, and a cheap mtime watch that reloads
//! the open page and picks up new ones.
use crate::diagrams::{self, Decision, Diagram};
use crate::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use nebula_core::{Agent, AgentId, Project, Worktree};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant, SystemTime},
};

const IMAGE_ID: u32 = 314159265;
/// Button-less pointer motion is forwarded at most this often (latest
/// position wins): the worker pays a compositor frame per move.
const HOVER_INTERVAL: Duration = Duration::from_millis(16);
/// How long the worker gets to close its browser before it is killed.
const CLOSE_GRACE: Duration = Duration::from_millis(1500);
/// Folder watch cadence: one `read_dir` plus a stat per diagram.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// A page gets one decision through per this long — a scripted loop must
/// not be able to flood the owning session with prompts.
const DECISION_INTERVAL: Duration = Duration::from_secs(1);
const BACKGROUND: Color = Color::Rgb(13, 17, 24);
const WORKER: &str = include_str!("../assets/html-viewer-worker.cjs");
const EMPTY: &str = "no diagrams — ask an agent for one (`nebula diagram new`)";
const HINT: &str = "Ctrl+scroll zoom · [ ] switch · / search · Ctrl+R reload · Esc or ⌘X closes";

fn viewer_area(frame: Rect) -> Rect {
    let width = (u32::from(frame.width) * 13 / 15) as u16;
    let height = (u32::from(frame.height) * 13 / 15) as u16;
    Rect::new(
        frame.x + (frame.width - width) / 2,
        frame.y + (frame.height - height) / 2,
        width,
        height,
    )
}

/// Does this worker line answer a frame/resize/goto request? Either the
/// frame, or the `reply`-tagged error the worker sends in its place.
fn answers_request(line: &Value) -> bool {
    line["png"].is_string() || line["reply"] == true
}

/// Wait up to `grace` for the child to exit by itself, then kill and reap it.
fn reap(mut child: Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// "now" / "5m" / "3h" / "12d" — the list strip has no room for more.
fn short_age(mtime: SystemTime) -> String {
    match mtime.elapsed().map_or(0, |d| d.as_secs()) {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg_attr(test, allow(dead_code))]
/// The embedded worker, written where node can run it. Rewritten only when
/// the content differs, so an upgraded binary replaces a stale copy.
fn worker_script() -> std::io::Result<PathBuf> {
    let dir = nebula_core::paths::runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("html-viewer-worker.cjs");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(WORKER) {
        std::fs::write(&path, WORKER)?;
    }
    Ok(path)
}

pub fn toggle(event: &Event) -> bool {
    matches!(event, Event::Key(k) if k.kind == KeyEventKind::Press
        && k.code == KeyCode::Char('x') && k.modifiers.contains(KeyModifiers::SUPER))
}

struct Renderer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Renderer {
    #[cfg_attr(test, allow(dead_code))]
    fn spawn(page: &Path) -> Result<Self, String> {
        let worker = worker_script().map_err(|e| format!("diagram viewer: {e}"))?;
        let mut child = Command::new("node")
            .arg(worker)
            .arg(page)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("diagram viewer needs node on PATH ({e})"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
        })
    }
}

/// `/` search: the query and the matching diagrams, best first.
struct Search {
    query: String,
    hits: Vec<usize>,
    sel: usize,
}

/// What became of a decision, for the event loop to type into the owner.
pub struct Delivery {
    pub agent: AgentId,
    pub prompt: String,
}

pub struct HtmlViewer {
    checkout: PathBuf,
    /// Newest first — the folder, and nothing but the folder.
    diagrams: Vec<Diagram>,
    /// Slug of the page on screen; None while the folder is empty.
    current: Option<String>,
    search: Option<Search>,
    /// `NEBULA_HTML_PROTOTYPE` test override: one fixed file, no folder.
    fixed: bool,
    renderer: Option<Renderer>,
    /// The renderer failed; Ctrl+R tries again, the folder watch does not.
    broken: bool,
    png: Option<String>,
    repaint: bool,
    /// Frame/resize/goto requests the worker has not answered yet.
    pending: u32,
    needs_frame: bool,
    last_request: Instant,
    /// Latest button-less pointer position not forwarded yet.
    hover: Option<(f64, f64)>,
    last_hover: Instant,
    last_scan: Instant,
    last_decision: Option<Instant>,
    /// Choices made on the page, waiting for [`Self::deliver`].
    decisions: Vec<(String, Decision)>,
    body: Rect,
    viewport: (u32, u32),
    status: String,
    /// Tests run without node: what would have gone to the worker.
    #[cfg(test)]
    sent: Vec<Value>,
    #[cfg(test)]
    spawned: bool,
}

impl HtmlViewer {
    /// Open on `checkout`'s diagrams: `slug` when given and present, else
    /// the newest. An empty folder still opens, with a message in the frame.
    pub fn start(checkout: PathBuf, slug: Option<&str>) -> Result<Self, String> {
        if std::env::var("TERM_PROGRAM").unwrap_or_default() != "ghostty" {
            return Err("diagram viewer needs Ghostty (Kitty graphics)".into());
        }
        let fixed = std::env::var_os("NEBULA_HTML_PROTOTYPE")
            .map(PathBuf::from)
            .filter(|p| p.is_file());
        Ok(Self::open(checkout, slug, fixed))
    }

    fn open(checkout: PathBuf, slug: Option<&str>, fixed: Option<PathBuf>) -> Self {
        let diagrams = match &fixed {
            Some(path) => vec![Diagram {
                slug: String::new(),
                title: path.display().to_string(),
                path: path.clone(),
                meta: Default::default(),
                mtime: SystemTime::UNIX_EPOCH,
            }],
            None => diagrams::scan(&checkout),
        };
        let mut viewer = Self {
            checkout,
            diagrams,
            current: None,
            search: None,
            fixed: fixed.is_some(),
            renderer: None,
            broken: false,
            png: None,
            repaint: false,
            pending: 0,
            needs_frame: false,
            last_request: Instant::now(),
            hover: None,
            last_hover: Instant::now(),
            last_scan: Instant::now(),
            last_decision: None,
            decisions: Vec::new(),
            body: Rect::default(),
            viewport: (1200, 800),
            status: EMPTY.into(),
            #[cfg(test)]
            sent: Vec::new(),
            #[cfg(test)]
            spawned: false,
        };
        let first = slug
            .and_then(|slug| viewer.diagrams.iter().position(|d| d.slug == slug))
            .unwrap_or(0);
        viewer.show(first);
        viewer
    }

    fn current(&self) -> Option<&Diagram> {
        let slug = self.current.as_ref()?;
        self.diagrams.iter().find(|d| &d.slug == slug)
    }

    fn connected(&self) -> bool {
        #[cfg(test)]
        if self.spawned {
            return !self.broken;
        }
        self.renderer.is_some()
    }

    /// Put `diagrams[index]` on screen. The first page starts the worker;
    /// every later one is a `goto` on the same Chromium page.
    fn show(&mut self, index: usize) {
        let Some(diagram) = self.diagrams.get(index) else {
            return;
        };
        let path = diagram.path.clone();
        self.current = Some(diagram.slug.clone());
        self.status = "Rendering…".into();
        if self.connected() {
            self.send(json!({"type":"goto","path":path}));
            self.pending += 1;
            return;
        }
        if self.broken {
            return;
        }
        #[cfg(test)]
        {
            self.spawned = true;
            self.sent.push(json!({"type":"spawn","path":path}));
        }
        #[cfg(not(test))]
        match Renderer::spawn(&path) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                self.broken = true;
                self.status = error;
                return;
            }
        }
        // A fresh worker knows nothing of the frame's size yet.
        if self.body.width > 0 {
            self.send_resize();
        }
    }

    fn step(&mut self, forward: bool) {
        let count = self.diagrams.len();
        let Some(at) = self.current().and_then(|c| self.diagrams.iter().position(|d| d.slug == c.slug)) else {
            return;
        };
        if count > 1 {
            self.show(if forward { (at + 1) % count } else { (at + count - 1) % count });
        }
    }

    /// Forward the held pointer position, if any. Everything else goes out
    /// through [`Self::send`], which flushes first, so the worker sees the
    /// same order the terminal produced.
    fn flush_hover(&mut self) {
        if let Some((x, y)) = self.hover.take() {
            self.last_hover = Instant::now();
            self.write(json!({"type":"mouse","action":"move","button":"left","x":x,"y":y,"dx":0,"dy":0}));
        }
    }
    fn send(&mut self, value: Value) {
        self.flush_hover();
        self.write(value);
    }
    /// Input for the page itself. Frames are demand-driven: only this asks
    /// for one, so an untouched document (or typing in the search box)
    /// costs no screenshots.
    fn send_input(&mut self, value: Value) {
        if self.current.is_some() {
            self.needs_frame = true;
            self.send(value);
        }
    }
    fn send_resize(&mut self) {
        self.send(json!({"type":"resize","width":self.viewport.0,"height":self.viewport.1}));
        self.pending += 1;
    }
    fn write(&mut self, value: Value) {
        #[cfg(test)]
        {
            self.sent.push(value.clone());
        }
        if let Some(stdin) = self.renderer.as_mut().and_then(|r| r.stdin.as_mut()) {
            if writeln!(stdin, "{value}").is_err() {
                self.status = "HTML renderer disconnected — Ctrl+R retries, Esc returns".into();
            }
        }
    }

    /// One line of worker output.
    fn handle_line(&mut self, line: &str) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        // Only the answer to a frame/resize/goto request settles one: the
        // frame itself, or the error standing in for it. A stray page error
        // is not a reply.
        if answers_request(&value) {
            self.pending = self.pending.saturating_sub(1);
        }
        if let Some(png) = value["png"].as_str() {
            // Worker output alone, not document-provided terminal bytes.
            if png.len() <= 16 * 1024 * 1024
                && png
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
            {
                self.repaint |= self.png.as_deref() != Some(png);
                self.png = Some(png.to_owned());
                if self.status == "Rendering…" {
                    self.status = HINT.into();
                }
            }
        }
        if let Some(error) = value["error"].as_str() {
            self.status = diagrams::clean(error, 200);
            self.broken |= value["fatal"] == true;
        }
        if value["type"] == "decision" {
            // Attributed by the worker to the file it was showing, never by
            // the page; a page that is not one of ours decides nothing.
            let slug = value["source"].as_str().and_then(|source| {
                let source = Path::new(source);
                self.diagrams
                    .iter()
                    .find(|d| d.path == source && !d.slug.is_empty())
                    .map(|d| d.slug.clone())
            });
            let throttled = self
                .last_decision
                .is_some_and(|at| at.elapsed() < DECISION_INTERVAL);
            if let (Some(slug), Some(decision), false) = (slug, Decision::from_value(&value), throttled) {
                self.last_decision = Some(Instant::now());
                self.decisions.push((slug, decision));
            }
            self.needs_frame = true;
        }
    }

    /// Save the pending decisions and work out who hears about them. Each
    /// is appended to the diagram's decisions log first; a [`Delivery`]
    /// comes back only for a diagram whose owning session is live on this
    /// checkout — nobody else is ever sent anything.
    pub fn deliver(
        &mut self,
        projects: &[Project],
        worktrees: &[Worktree],
        agents: &[Agent],
        now_ms: i64,
    ) -> Vec<Delivery> {
        let mut deliveries = Vec::new();
        for (slug, decision) in std::mem::take(&mut self.decisions) {
            let log = match diagrams::append_decision(&self.checkout, &slug, &decision, now_ms) {
                Ok(log) => log,
                Err(error) => {
                    self.status = format!("decision not saved: {error}");
                    continue;
                }
            };
            let owner = self
                .diagrams
                .iter()
                .find(|d| d.slug == slug)
                .and_then(|d| diagrams::resolve_owner(d, &self.checkout, projects, worktrees, agents));
            self.status = match owner {
                Some(agent) => {
                    deliveries.push(Delivery {
                        agent: agent.id.clone(),
                        prompt: decision.prompt(&slug, &log),
                    });
                    format!("\"{}\" sent to {}", decision.choice, diagrams::clean(&agent.name, 60))
                }
                None => format!("\"{}\" saved (no live owner session)", decision.choice),
            };
        }
        deliveries
    }

    /// Folder watch: pick up new, changed and removed diagrams. True when
    /// anything moved.
    fn rescan(&mut self) -> bool {
        self.last_scan = Instant::now();
        if self.fixed {
            return false;
        }
        let after = diagrams::scan_reusing(&self.checkout, &self.diagrams);
        if after == self.diagrams {
            return false;
        }
        let shown = self.current().cloned();
        let before = std::mem::replace(&mut self.diagrams, after);
        if let Some(search) = &mut self.search {
            search.hits = diagrams::search(&search.query, &self.diagrams);
            search.sel = search.sel.min(search.hits.len().saturating_sub(1));
        }
        self.repaint = true;
        let Some(shown) = shown else {
            // The folder was empty until now.
            self.show(0);
            return true;
        };
        if let Some(next) = diagrams::auto_focus(&shown, &before, &self.diagrams) {
            self.show(next);
            return true;
        }
        match self.diagrams.iter().find(|d| d.slug == shown.slug) {
            Some(now) if now.mtime != shown.mtime => {
                self.send(json!({"type":"reload"}));
                self.needs_frame = true;
            }
            Some(_) => {}
            None if self.diagrams.is_empty() => {
                self.current = None;
                self.png = None;
                self.status = EMPTY.into();
            }
            None => self.show(0),
        }
        true
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let lines: Vec<String> = match &self.renderer {
            Some(renderer) => renderer.lines.try_iter().collect(),
            None => Vec::new(),
        };
        for line in lines {
            self.handle_line(&line);
            changed = true;
        }
        if let Some(renderer) = &mut self.renderer {
            if matches!(renderer.child.try_wait(), Ok(Some(_))) {
                // A fatal error already said why (no Playwright, no Chrome).
                if !self.broken {
                    self.status = "HTML renderer exited — Ctrl+R retries, Esc returns".into();
                }
                self.broken = true;
                self.renderer = None;
                self.pending = 0;
                changed = true;
            }
        }
        if self.last_scan.elapsed() >= SCAN_INTERVAL {
            changed |= self.rescan();
        }
        if self.last_hover.elapsed() >= HOVER_INTERVAL {
            self.flush_hover();
        }
        if self.renderer.is_some()
            && self.needs_frame
            && self.pending == 0
            && self.body.width > 0
            && self.last_request.elapsed() >= Duration::from_millis(80)
        {
            self.send(json!({"type":"frame"}));
            self.needs_frame = false;
            self.pending += 1;
            self.last_request = Instant::now();
        }
        changed
    }

    /// True while decisions wait for [`Self::deliver`].
    pub fn has_decisions(&self) -> bool {
        !self.decisions.is_empty()
    }

    /// The diagrams the strip lists: the search hits while searching.
    fn listed(&self) -> Vec<usize> {
        match &self.search {
            Some(search) => search.hits.clone(),
            None => (0..self.diagrams.len()).collect(),
        }
    }

    /// One row of `title · age` entries, windowed so the highlighted one
    /// (the search selection, else the page on screen) is always visible.
    fn strip(&self, width: u16, theme: &Theme) -> Line<'static> {
        let listed = self.listed();
        if listed.is_empty() {
            return Line::styled(" no match", Style::default().fg(theme.dim));
        }
        let marked = match &self.search {
            Some(search) => search.sel,
            None => listed
                .iter()
                .position(|&i| Some(&self.diagrams[i].slug) == self.current.as_ref())
                .unwrap_or(0),
        };
        let items: Vec<Span> = listed
            .iter()
            .enumerate()
            .map(|(n, &i)| {
                let d = &self.diagrams[i];
                let title: String = d.title.chars().take(28).collect();
                let style = if n == marked {
                    Style::default()
                        .fg(theme.accent)
                        .bg(theme.sel_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted)
                };
                Span::styled(format!(" {title} · {} ", short_age(d.mtime)), style)
            })
            .collect();
        // Slide the window right until the marked entry fits.
        let room = usize::from(width).saturating_sub(4);
        let mut start = 0;
        while start < marked
            && items[start..=marked].iter().map(Span::width).sum::<usize>() > room
        {
            start += 1;
        }
        let edge = Style::default().fg(theme.dim);
        let mut spans = vec![Span::styled(if start > 0 { "‹ " } else { "  " }, edge)];
        let mut used = 0;
        let mut cut = false;
        for (n, item) in items[start..].iter().enumerate() {
            used += item.width();
            // The first entry always shows, however narrow the frame.
            if used > room && n > 0 {
                cut = true;
                break;
            }
            spans.push(item.clone());
        }
        if cut {
            spans.push(Span::styled(" ›", edge));
        }
        Line::from(spans)
    }

    pub fn draw(&mut self, f: &mut Frame, theme: &Theme) {
        let area = viewer_area(f.area());
        let title = match self.current() {
            Some(d) if !d.slug.is_empty() => format!(" ◇ {} — {} ", d.title, d.slug),
            Some(d) => format!(" ◇ {} ", d.title),
            None => " ◇ Diagrams ".into(),
        };
        f.render_widget(Clear, area);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .style(Style::default().bg(BACKGROUND).fg(theme.accent)),
            area,
        );
        let inner_width = area.width.saturating_sub(2);
        let head = match &self.search {
            Some(search) => Line::from(vec![
                Span::styled("/", Style::default().fg(theme.accent)),
                Span::styled(search.query.clone(), Style::default().fg(theme.text)),
                Span::styled("▏", Style::default().fg(theme.accent)),
                Span::styled(
                    "  Enter opens · ←/→ picks · Esc leaves search",
                    Style::default().fg(theme.dim),
                ),
            ]),
            None => Line::styled(self.status.clone(), Style::default().fg(theme.muted)),
        };
        f.render_widget(Paragraph::new(head), Rect::new(area.x + 1, area.y + 1, inner_width, 1));
        // The strip only takes a row when there is something to switch to.
        let strip = u16::from(self.diagrams.len() > 1 || self.search.is_some());
        if strip == 1 {
            f.render_widget(
                Paragraph::new(self.strip(inner_width, theme)),
                Rect::new(area.x + 1, area.y + 2, inner_width, 1),
            );
        }
        let body = Rect::new(
            area.x + 1,
            area.y + 2 + strip,
            inner_width,
            area.height.saturating_sub(3 + strip),
        );
        if self.current.is_none() && body.height > 0 {
            f.render_widget(
                Paragraph::new(EMPTY)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(theme.muted)),
                Rect::new(body.x, body.y + body.height / 2, body.width, 1),
            );
        }
        if self.body != body && body.width > 0 && body.height > 0 {
            self.body = body;
            let size = crossterm::terminal::window_size().ok();
            let cw = size
                .as_ref()
                .filter(|s| s.columns > 0 && s.width > 0)
                .map(|s| s.width as f64 / s.columns as f64)
                .unwrap_or(9.0);
            let ch = size
                .as_ref()
                .filter(|s| s.rows > 0 && s.height > 0)
                .map(|s| s.height as f64 / s.rows as f64)
                .unwrap_or(18.0);
            // Cap raster pressure; preserve aspect ratio, including Retina terminals.
            let w = body.width as f64 * cw;
            let h = body.height as f64 * ch;
            let scale = (1600.0 / w).min(1100.0 / h).min(1.0);
            self.viewport = ((w * scale).max(1.0) as u32, (h * scale).max(1.0) as u32);
            if self.connected() {
                self.send_resize();
            }
            self.repaint = true;
        }
    }

    pub fn paint(&mut self, out: &mut impl Write) -> std::io::Result<()> {
        if !self.repaint {
            return Ok(());
        }
        self.repaint = false;
        let Some(png) = &self.png else {
            // Nothing to show (any more): take our image down.
            return Self::clear(out);
        };
        write!(out, "\x1b7\x1b[{};{}H", self.body.y + 1, self.body.x + 1)?;
        // Replace only our own image; never delete another client's images.
        Self::clear(out)?;
        let chunks: Vec<_> = png.as_bytes().chunks(4096).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let more = usize::from(i + 1 < chunks.len());
            if i == 0 {
                write!(
                    out,
                    "\x1b_Ga=T,f=100,i={IMAGE_ID},c={},r={},C=1,z=1,q=2,m={more};",
                    self.body.width, self.body.height
                )?;
            } else {
                write!(out, "\x1b_Gq=2,m={more};")?;
            }
            out.write_all(chunk)?;
            out.write_all(b"\x1b\\")?;
        }
        out.write_all(b"\x1b8")?;
        out.flush()
    }

    pub fn clear(out: &mut impl Write) -> std::io::Result<()> {
        write!(out, "\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\")?;
        out.flush()
    }

    fn search_changed(&mut self) {
        if let Some(search) = &mut self.search {
            search.hits = diagrams::search(&search.query, &self.diagrams);
            search.sel = 0;
        }
    }

    /// Search mode owns the keyboard: nothing typed here reaches the page.
    fn search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let Some(search) = &mut self.search else {
            return;
        };
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let last = search.hits.len().saturating_sub(1);
        match code {
            KeyCode::Esc => self.search = None,
            KeyCode::Enter => {
                let pick = search.hits.get(search.sel).copied();
                self.search = None;
                if let Some(index) = pick {
                    self.show(index);
                }
            }
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab => search.sel = search.sel.saturating_sub(1),
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => search.sel = (search.sel + 1).min(last),
            KeyCode::Char('k' | 'p') if ctrl => search.sel = search.sel.saturating_sub(1),
            KeyCode::Char('j' | 'n') if ctrl => search.sel = (search.sel + 1).min(last),
            KeyCode::Backspace => {
                search.query.pop();
                self.search_changed();
            }
            KeyCode::Char(c)
                if !modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                search.query.push(c);
                self.search_changed();
            }
            _ => {}
        }
    }

    /// True closes the viewer. All other input stays out of the agent PTY.
    pub fn event(&mut self, event: Event) -> bool {
        if toggle(&event) {
            return true;
        }
        match event {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if self.search.is_some() {
                    self.search_key(k.code, k.modifiers);
                    return false;
                }
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let bare = !k.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                );
                match k.code {
                    KeyCode::Esc => return true,
                    KeyCode::Char('r') if ctrl => {
                        if self.connected() {
                            self.send_input(json!({"type":"reload"}));
                        } else if let Some(at) = self
                            .current()
                            .and_then(|c| self.diagrams.iter().position(|d| d.slug == c.slug))
                        {
                            // The renderer is gone: start it again.
                            self.broken = false;
                            self.show(at);
                        }
                        return false;
                    }
                    KeyCode::Char('/') if bare => {
                        self.search = Some(Search {
                            query: String::new(),
                            hits: (0..self.diagrams.len()).collect(),
                            sel: 0,
                        });
                        return false;
                    }
                    KeyCode::Char(']') if bare => {
                        self.step(true);
                        return false;
                    }
                    KeyCode::Char('[') if bare => {
                        self.step(false);
                        return false;
                    }
                    KeyCode::Char('j') if ctrl => {
                        self.step(true);
                        return false;
                    }
                    KeyCode::Char('k') if ctrl => {
                        self.step(false);
                        return false;
                    }
                    _ => {}
                }
                let name = match k.code {
                    KeyCode::Char(c) => c.to_string(),
                    KeyCode::Enter => "Enter".into(),
                    KeyCode::Tab => "Tab".into(),
                    KeyCode::BackTab => "Shift+Tab".into(),
                    KeyCode::Backspace => "Backspace".into(),
                    KeyCode::Delete => "Delete".into(),
                    KeyCode::Left => "ArrowLeft".into(),
                    KeyCode::Right => "ArrowRight".into(),
                    KeyCode::Up => "ArrowUp".into(),
                    KeyCode::Down => "ArrowDown".into(),
                    KeyCode::Home => "Home".into(),
                    KeyCode::End => "End".into(),
                    KeyCode::PageUp => "PageUp".into(),
                    KeyCode::PageDown => "PageDown".into(),
                    _ => return false,
                };
                let mut prefix = String::new();
                for (flag, text) in [
                    (KeyModifiers::CONTROL, "Control+"),
                    (KeyModifiers::SUPER, "Meta+"),
                    (KeyModifiers::ALT, "Alt+"),
                ] {
                    if k.modifiers.contains(flag) {
                        prefix.push_str(text);
                    }
                }
                self.send_input(json!({"type":"key","key":format!("{prefix}{name}")}));
            }
            Event::Paste(text) => match &mut self.search {
                Some(search) => {
                    search.query.push_str(&diagrams::clean(&text, 64));
                    self.search_changed();
                }
                None => self.send_input(json!({"type":"text","text":text})),
            },
            Event::Mouse(m) if self.current.is_some() && self.body.contains((m.column, m.row).into()) => {
                if m.modifiers.contains(KeyModifiers::CONTROL) {
                    let zoom_key = match m.kind {
                        MouseEventKind::ScrollUp => Some("+"),
                        MouseEventKind::ScrollDown => Some("-"),
                        _ => None,
                    };
                    if let Some(key) = zoom_key {
                        self.send_input(json!({"type":"key","key":key}));
                        return false;
                    }
                }
                let x = ((m.column - self.body.x) as f64 + 0.5) / self.body.width.max(1) as f64
                    * self.viewport.0 as f64;
                let y = ((m.row - self.body.y) as f64 + 0.5) / self.body.height.max(1) as f64
                    * self.viewport.1 as f64;
                let (action, button, dx, dy) = match m.kind {
                    MouseEventKind::Down(b) => ("down", b, 0, 0),
                    MouseEventKind::Up(b) => ("up", b, 0, 0),
                    MouseEventKind::Drag(b) => ("move", b, 0, 0),
                    MouseEventKind::Moved => ("move", MouseButton::Left, 0, 0),
                    MouseEventKind::ScrollUp => ("wheel", MouseButton::Left, 0, -100),
                    MouseEventKind::ScrollDown => ("wheel", MouseButton::Left, 0, 100),
                    MouseEventKind::ScrollLeft => ("wheel", MouseButton::Left, -100, 0),
                    MouseEventKind::ScrollRight => ("wheel", MouseButton::Left, 100, 0),
                };
                if m.kind == MouseEventKind::Moved {
                    self.needs_frame = true;
                    self.hover = Some((x, y));
                    if self.last_hover.elapsed() >= HOVER_INTERVAL {
                        self.flush_hover();
                    }
                    return false;
                }
                let button = match button {
                    MouseButton::Left => "left",
                    MouseButton::Right => "right",
                    MouseButton::Middle => "middle",
                };
                self.send_input(json!({"type":"mouse","action":action,"button":button,"x":x,"y":y,"dx":dx,"dy":dy}));
            }
            _ => {}
        }
        false
    }
}

impl Drop for HtmlViewer {
    fn drop(&mut self) {
        self.send(json!({"type":"close"}));
        // Reap our worker after it closes its isolated browser — and kill it
        // when it doesn't, so no renderer outlives the viewer. Never block
        // TUI exit.
        if let Some(mut renderer) = self.renderer.take() {
            renderer.stdin.take();
            std::thread::spawn(move || reap(renderer.child, CLOSE_GRACE));
        }
        #[cfg(not(test))]
        let _ = Self::clear(&mut std::io::stdout());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn only_a_frame_or_its_stand_in_error_settles_a_request() {
        assert!(answers_request(&json!({"png":"AAAA","width":1,"height":1})));
        assert!(answers_request(&json!({"error":"screenshot timed out","reply":true})));
        assert!(!answers_request(&json!({"error":"ReferenceError: x is not defined"})));
    }

    #[test]
    fn a_worker_that_ignores_close_is_killed_after_the_grace_period() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id().to_string();
        let started = Instant::now();
        reap(child, Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(5));
        // Reaped, so the pid is gone (or at least no longer our child).
        let alive = Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap();
        assert!(!alive.success());
    }

    #[test]
    fn a_worker_that_exits_on_its_own_is_reaped_without_waiting_out_the_grace() {
        let child = Command::new("true").spawn().unwrap();
        let started = Instant::now();
        reap(child, Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn viewer_occupies_thirteen_fifteenths_and_stays_centered() {
        assert_eq!(viewer_area(Rect::new(0, 0, 180, 60)), Rect::new(12, 4, 156, 52));
        assert_eq!(viewer_area(Rect::new(5, 7, 100, 40)), Rect::new(12, 10, 86, 34));
    }

    /// A diagram `age_secs` old, owned by `agent` when given.
    fn diagram(checkout: &Path, slug: &str, title: &str, agent: Option<&str>, age_secs: u64) -> PathBuf {
        let meta = diagrams::Meta {
            tags: vec![],
            session: None,
            agent: agent.map(str::to_owned),
        };
        let path = diagrams::file(checkout, slug);
        std::fs::create_dir_all(diagrams::dir(checkout)).unwrap();
        std::fs::write(&path, diagrams::render_template(diagrams::Template::Plain, slug, title, &meta)).unwrap();
        touch(&path, age_secs);
        path
    }

    fn touch(path: &Path, age_secs: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(age_secs)).unwrap();
    }

    /// Three diagrams, newest first: flow, storage, deploy.
    fn three() -> (tempfile::TempDir, HtmlViewer) {
        let tmp = tempfile::tempdir().unwrap();
        diagram(tmp.path(), "deploy", "Release pipeline", None, 300);
        diagram(tmp.path(), "storage", "Storage layout", None, 200);
        diagram(tmp.path(), "flow", "Login flow", None, 100);
        let viewer = HtmlViewer::open(tmp.path().to_path_buf(), None, None);
        (tmp, viewer)
    }

    fn key(viewer: &mut HtmlViewer, code: KeyCode, modifiers: KeyModifiers) -> bool {
        viewer.event(Event::Key(KeyEvent::new(code, modifiers)))
    }

    fn typed(viewer: &mut HtmlViewer, text: &str) {
        for c in text.chars() {
            assert!(!key(viewer, KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// What went to the worker since the last call, as `type[:detail]`.
    fn sent(viewer: &mut HtmlViewer) -> Vec<String> {
        std::mem::take(&mut viewer.sent)
            .iter()
            .map(|m| {
                let kind = m["type"].as_str().unwrap().to_owned();
                match m["path"].as_str().or(m["key"].as_str()) {
                    Some(path) if kind == "spawn" || kind == "goto" => format!(
                        "{kind}:{}",
                        Path::new(path).file_stem().unwrap().to_string_lossy()
                    ),
                    Some(key) => format!("{kind}:{key}"),
                    None => kind,
                }
            })
            .collect()
    }

    #[test]
    fn control_scroll_zooms_while_plain_scroll_still_scrolls() {
        let (_tmp, mut viewer) = three();
        viewer.body = Rect::new(0, 0, 100, 40);
        sent(&mut viewer);
        for (kind, modifiers) in [
            (MouseEventKind::ScrollUp, KeyModifiers::CONTROL),
            (MouseEventKind::ScrollDown, KeyModifiers::CONTROL),
            (MouseEventKind::ScrollDown, KeyModifiers::NONE),
        ] {
            viewer.event(Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 10,
                row: 10,
                modifiers,
            }));
        }
        assert_eq!(sent(&mut viewer), ["key:+", "key:-", "mouse"]);
    }

    #[test]
    fn opens_the_newest_diagram_or_the_one_asked_for() {
        let (tmp, mut viewer) = three();
        assert_eq!(viewer.current.as_deref(), Some("flow"));
        assert_eq!(sent(&mut viewer), ["spawn:flow"]);

        let mut asked = HtmlViewer::open(tmp.path().to_path_buf(), Some("deploy"), None);
        assert_eq!(sent(&mut asked), ["spawn:deploy"]);
        // A slug that is gone by now falls back to the newest.
        let mut stale = HtmlViewer::open(tmp.path().to_path_buf(), Some("gone"), None);
        assert_eq!(sent(&mut stale), ["spawn:flow"]);
    }

    #[test]
    fn an_empty_folder_opens_with_a_message_and_starts_no_renderer() {
        let tmp = tempfile::tempdir().unwrap();
        let mut viewer = HtmlViewer::open(tmp.path().to_path_buf(), None, None);
        assert_eq!(viewer.current, None);
        assert_eq!(viewer.status, EMPTY);
        // Keys have no page to go to; Esc still closes.
        typed(&mut viewer, "abc]");
        assert!(sent(&mut viewer).is_empty());
        assert!(key(&mut viewer, KeyCode::Esc, KeyModifiers::NONE));

        // The first diagram to appear is rendered without reopening.
        diagram(tmp.path(), "first", "First", None, 0);
        assert!(viewer.rescan());
        assert_eq!(sent(&mut viewer), ["spawn:first"]);
    }

    #[test]
    fn brackets_and_ctrl_jk_switch_on_the_same_page_and_wrap() {
        let (_tmp, mut viewer) = three();
        sent(&mut viewer);
        typed(&mut viewer, "]");
        key(&mut viewer, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(viewer.current.as_deref(), Some("deploy"));
        typed(&mut viewer, "]");
        assert_eq!(viewer.current.as_deref(), Some("flow"), "wraps past the oldest");
        typed(&mut viewer, "[");
        key(&mut viewer, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(viewer.current.as_deref(), Some("storage"));
        // Every switch is a goto on the running worker: no restart, and the
        // switch keys themselves never reach the page.
        assert_eq!(
            sent(&mut viewer),
            ["goto:storage", "goto:deploy", "goto:flow", "goto:deploy", "goto:storage"]
        );
        assert_eq!(viewer.pending, 5, "each goto is owed a frame");
    }

    #[test]
    fn other_keys_go_to_the_page_and_ask_for_a_frame() {
        let (_tmp, mut viewer) = three();
        sent(&mut viewer);
        typed(&mut viewer, "a");
        key(&mut viewer, KeyCode::Down, KeyModifiers::NONE);
        key(&mut viewer, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(sent(&mut viewer), ["key:a", "key:ArrowDown", "key:Control+a"]);
        assert!(viewer.needs_frame);
        // One diagram only: the switch keys have nowhere to go.
        let tmp = tempfile::tempdir().unwrap();
        diagram(tmp.path(), "solo", "Solo", None, 0);
        let mut solo = HtmlViewer::open(tmp.path().to_path_buf(), None, None);
        sent(&mut solo);
        typed(&mut solo, "][");
        assert!(sent(&mut solo).is_empty());
    }

    #[test]
    fn search_filters_live_keeps_keys_from_the_page_and_enter_renders() {
        let (_tmp, mut viewer) = three();
        sent(&mut viewer);
        typed(&mut viewer, "/");
        assert_eq!(viewer.listed(), [0, 1, 2], "an empty query lists everything");
        typed(&mut viewer, "stor");
        assert_eq!(viewer.listed(), [1]);
        key(&mut viewer, KeyCode::Backspace, KeyModifiers::NONE);
        typed(&mut viewer, "]");
        assert!(viewer.listed().is_empty(), "`]` is query text here, not a switch");
        assert_eq!(viewer.current.as_deref(), Some("flow"));
        for _ in 0..2 {
            key(&mut viewer, KeyCode::Backspace, KeyModifiers::NONE);
        }
        typed(&mut viewer, "r");
        assert_eq!(viewer.listed(), [1], "sto + r");
        viewer.event(Event::Paste("ignored\u{1b}".into()));
        key(&mut viewer, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(sent(&mut viewer).is_empty(), "nothing typed in search reaches the page");
        assert!(!viewer.needs_frame, "and none of it costs a screenshot");

        // Enter renders the selection and leaves search.
        for _ in 0.."ignored".len() {
            key(&mut viewer, KeyCode::Backspace, KeyModifiers::NONE);
        }
        assert!(!key(&mut viewer, KeyCode::Enter, KeyModifiers::NONE));
        assert!(viewer.search.is_none());
        assert_eq!(sent(&mut viewer), ["goto:storage"]);
    }

    #[test]
    fn arrows_pick_among_the_hits_and_enter_without_a_hit_changes_nothing() {
        let (_tmp, mut viewer) = three();
        sent(&mut viewer);
        typed(&mut viewer, "/lo");
        // "lo" hits Login flow and Storage layout (and deploy's "pLOy").
        assert!(viewer.listed().len() >= 2);
        let second = viewer.listed()[1];
        key(&mut viewer, KeyCode::Right, KeyModifiers::NONE);
        key(&mut viewer, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(viewer.current.as_deref(), Some(viewer.diagrams[second].slug.as_str()));

        sent(&mut viewer);
        typed(&mut viewer, "/zzz");
        key(&mut viewer, KeyCode::Enter, KeyModifiers::NONE);
        assert!(sent(&mut viewer).is_empty());
    }

    #[test]
    fn the_first_esc_leaves_search_and_the_second_closes() {
        let (_tmp, mut viewer) = three();
        typed(&mut viewer, "/sto");
        assert!(!key(&mut viewer, KeyCode::Esc, KeyModifiers::NONE));
        assert!(viewer.search.is_none());
        assert_eq!(viewer.current.as_deref(), Some("flow"), "leaving search switches nothing");
        assert!(key(&mut viewer, KeyCode::Esc, KeyModifiers::NONE));
        // ⌘X closes from anywhere, search included.
        typed(&mut viewer, "/");
        assert!(key(&mut viewer, KeyCode::Char('x'), KeyModifiers::SUPER));
    }

    #[test]
    fn the_open_page_reloads_when_its_mtime_moves() {
        let (tmp, mut viewer) = three();
        sent(&mut viewer);
        assert!(!viewer.rescan(), "a quiet folder changes nothing");
        assert!(sent(&mut viewer).is_empty());

        // Another diagram changing updates the list only.
        touch(&diagrams::file(tmp.path(), "deploy"), 250);
        assert!(viewer.rescan());
        assert!(sent(&mut viewer).is_empty());

        touch(&diagrams::file(tmp.path(), "flow"), 1);
        assert!(viewer.rescan());
        assert_eq!(sent(&mut viewer), ["reload"]);
        assert!(viewer.needs_frame);
    }

    #[test]
    fn new_and_removed_diagrams_update_the_list() {
        let (tmp, mut viewer) = three();
        sent(&mut viewer);
        // Unowned pages never steal the focus, however new.
        diagram(tmp.path(), "extra", "Extra", None, 0);
        assert!(viewer.rescan());
        assert_eq!(viewer.diagrams.len(), 4);
        assert_eq!(viewer.current.as_deref(), Some("flow"));
        assert!(sent(&mut viewer).is_empty());

        // The shown one disappears: fall back to the newest.
        std::fs::remove_file(diagrams::file(tmp.path(), "flow")).unwrap();
        assert!(viewer.rescan());
        assert_eq!(sent(&mut viewer), ["goto:extra"]);

        // Everything disappears: the image goes, the message shows.
        std::fs::remove_dir_all(diagrams::dir(tmp.path())).unwrap();
        viewer.png = Some("AAAA".into());
        assert!(viewer.rescan());
        assert_eq!((viewer.current.clone(), viewer.png.clone()), (None, None));
        let mut out = Vec::new();
        viewer.paint(&mut out).unwrap();
        assert_eq!(out, format!("\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\").into_bytes());
    }

    #[test]
    fn a_follow_up_page_by_the_same_session_takes_the_focus() {
        let tmp = tempfile::tempdir().unwrap();
        diagram(tmp.path(), "ask", "Which database?", Some("s1"), 100);
        let mut viewer = HtmlViewer::open(tmp.path().to_path_buf(), None, None);
        sent(&mut viewer);
        diagram(tmp.path(), "noise", "Someone else", Some("s2"), 5);
        assert!(viewer.rescan());
        assert_eq!(viewer.current.as_deref(), Some("ask"));
        diagram(tmp.path(), "answer", "Schema for Postgres", Some("s1"), 1);
        assert!(viewer.rescan());
        assert_eq!(sent(&mut viewer), ["goto:answer"]);
        // Going back on purpose sticks: nothing changed, nothing refocuses.
        typed(&mut viewer, "]]");
        assert_eq!(viewer.current.as_deref(), Some("ask"));
        assert!(!viewer.rescan());
        assert_eq!(viewer.current.as_deref(), Some("ask"));
    }

    fn decision_line(path: &Path, choice: &str) -> String {
        json!({"type":"decision","source":path,"id":"db","choice":choice,"label":"Postgres","note":""})
            .to_string()
    }

    #[test]
    fn a_decision_is_saved_and_delivered_to_its_live_owner_only() {
        use nebula_core::{AgentKind, AgentStatus, ProjectId, WorktreeId};
        let tmp = tempfile::tempdir().unwrap();
        let owned = diagram(tmp.path(), "pick", "Pick", Some("a1"), 10);
        let orphan = diagram(tmp.path(), "orphan", "Orphan", None, 20);
        let project: Project = serde_json::from_value(json!({
            "id":"p","name":"p","repo_path":tmp.path(),"sort_order":0,
            "divider_after":false,"divider_label":null,"divider_before":false,"divider_before_label":null
        }))
        .unwrap();
        let worktree = Worktree {
            id: WorktreeId("w".into()),
            project_id: ProjectId("p".into()),
            path: tmp.path().to_path_buf(),
            branch: "main".into(),
            is_main: true,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        let mut agent = Agent {
            id: AgentId("a1".into()),
            worktree_id: WorktreeId("w".into()),
            name: "designer".into(),
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
            alive: true,
            branch: None,
        };
        let mut viewer = HtmlViewer::open(tmp.path().to_path_buf(), None, None);

        viewer.handle_line(&decision_line(&owned, "pg"));
        assert!(viewer.has_decisions() && viewer.needs_frame);
        let out = viewer.deliver(&[project.clone()], &[worktree.clone()], &[agent.clone()], 42);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].agent, agent.id);
        assert!(out[0].prompt.starts_with("[nebula diagram] pick: decision \"db\" = \"pg\" (Postgres). Decisions file: "));
        assert_eq!(viewer.status, "\"pg\" sent to designer");
        assert_eq!(diagrams::read_decisions(tmp.path(), "pick")[0]["ts"], 42);

        // A second decision inside the throttle window is dropped entirely.
        viewer.handle_line(&decision_line(&owned, "again"));
        assert!(!viewer.has_decisions());

        // Dead owner, or no owner at all: saved, sent to nobody.
        agent.alive = false;
        for (path, slug) in [(&owned, "pick"), (&orphan, "orphan")] {
            viewer.last_decision = None;
            viewer.handle_line(&decision_line(path, "sqlite"));
            let out = viewer.deliver(&[project.clone()], &[worktree.clone()], &[agent.clone()], 43);
            assert!(out.is_empty(), "{slug}");
            assert_eq!(viewer.status, "\"sqlite\" saved (no live owner session)");
            assert_eq!(diagrams::read_decisions(tmp.path(), slug).last().unwrap()["choice"], "sqlite");
        }

        // A page outside the folder decides nothing.
        viewer.last_decision = None;
        viewer.handle_line(&decision_line(&tmp.path().join("evil.html"), "x"));
        assert!(!viewer.has_decisions());
    }

    #[test]
    fn worker_errors_become_one_clean_status_line_and_fatal_ones_stop_retries() {
        let (tmp, mut viewer) = three();
        viewer.pending = 1;
        viewer.handle_line(r#"{"error":"ReferenceError: x\u001b[31m is not defined"}"#);
        assert_eq!(viewer.status, "ReferenceError: x[31m is not defined");
        assert_eq!(viewer.pending, 1, "a stray page error settles nothing");
        viewer.handle_line(r#"{"error":"Playwright not found","fatal":true}"#);
        assert!(viewer.broken);
        // The folder watch does not respawn a broken renderer…
        sent(&mut viewer);
        diagram(tmp.path(), "later", "Later", None, 0);
        std::fs::remove_file(diagrams::file(tmp.path(), "flow")).unwrap();
        viewer.rescan();
        assert!(sent(&mut viewer).is_empty());
        // …Ctrl+R does.
        key(&mut viewer, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(!viewer.broken);
    }

    #[test]
    fn the_strip_keeps_the_current_diagram_in_view() {
        let (_tmp, mut viewer) = three();
        let theme = Theme::by_name("default");
        let text = |line: Line| line.spans.iter().map(|s| s.content.to_string()).collect::<String>();
        let wide = text(viewer.strip(120, &theme));
        assert!(wide.contains("Login flow · 1m") && wide.contains("Release pipeline · 5m"), "{wide}");
        typed(&mut viewer, "]]");
        let narrow = text(viewer.strip(30, &theme));
        assert!(narrow.starts_with("‹ ") && narrow.contains("Release pipeline"), "{narrow}");
    }
}

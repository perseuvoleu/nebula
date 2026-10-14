//! THROWAWAY: an actual offscreen HTML document inside the outer Ghostty TUI.
//! Not a native WebView: Chromium produces frames, Kitty graphics displays them,
//! and terminal input is forwarded to the live DOM. No daemon/PTY modifications.
//! Worker/sample currently live in this checkout's ignored `.scratch/` directory.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

const IMAGE_ID: u32 = 314159265;
/// Button-less pointer motion is forwarded at most this often (latest
/// position wins): the worker pays a compositor frame per move.
const HOVER_INTERVAL: Duration = Duration::from_millis(16);
/// How long the worker gets to close its browser before it is killed.
const CLOSE_GRACE: Duration = Duration::from_millis(1500);

fn viewer_area(frame: Rect) -> Rect {
    let width = (u32::from(frame.width) * 2 / 3) as u16;
    let height = (u32::from(frame.height) * 2 / 3) as u16;
    Rect::new(
        frame.x + (frame.width - width) / 2,
        frame.y + (frame.height - height) / 2,
        width,
        height,
    )
}

/// Does this worker line answer a frame/resize request? Either the frame,
/// or the `reply`-tagged error the worker sends in its place.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_frame_or_its_stand_in_error_settles_a_request() {
        assert!(answers_request(&json!({"png":"AAAA","width":1,"height":1})));
        assert!(answers_request(
            &json!({"error":"screenshot timed out","reply":true})
        ));
        assert!(!answers_request(
            &json!({"error":"ReferenceError: x is not defined"})
        ));
    }

    #[test]
    fn a_worker_that_ignores_close_is_killed_after_the_grace_period() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id().to_string();
        let started = Instant::now();
        reap(child, Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(5));
        // Reaped, so the pid is gone (or at least no longer our child).
        let alive = Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap();
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
    fn viewer_occupies_two_thirds_and_stays_centered() {
        assert_eq!(
            viewer_area(Rect::new(0, 0, 180, 60)),
            Rect::new(30, 10, 120, 40)
        );
        assert_eq!(
            viewer_area(Rect::new(5, 7, 100, 40)),
            Rect::new(22, 14, 66, 26)
        );
    }
}
pub fn toggle(event: &Event) -> bool {
    matches!(event, Event::Key(k) if k.kind == KeyEventKind::Press
        && k.code == KeyCode::Char('x') && k.modifiers.contains(KeyModifiers::SUPER))
}

pub struct HtmlPrototype {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    frames: Receiver<String>,
    png: Option<String>,
    repaint: bool,
    /// Frame/resize requests the worker has not answered yet.
    pending: u32,
    needs_frame: bool,
    last_request: Instant,
    /// Latest button-less pointer position not forwarded yet.
    hover: Option<(f64, f64)>,
    last_hover: Instant,
    body: Rect,
    viewport: (u32, u32),
    status: String,
}
impl HtmlPrototype {
    pub fn start() -> Result<Self, String> {
        if std::env::var("TERM_PROGRAM").unwrap_or_default() != "ghostty" {
            return Err("HTML prototype currently needs Ghostty (Kitty graphics)".into());
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let worker = root.join(".scratch/native-html/worker.cjs");
        let html = std::env::var_os("NEBULA_HTML_PROTOTYPE")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(".scratch/diagram-viewer/sample.html"));
        if !worker.is_file() || !html.is_file() {
            return Err("HTML prototype worker/sample missing from .scratch".into());
        }
        let mut child = Command::new("node")
            .arg(worker)
            .arg(html)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("HTML prototype: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child: Some(child),
            stdin,
            frames,
            png: None,
            repaint: false,
            pending: 0,
            needs_frame: false,
            last_request: Instant::now(),
            hover: None,
            last_hover: Instant::now(),
            body: Rect::default(),
            viewport: (1200, 800),
            status: "Starting headless HTML renderer…".into(),
        })
    }
    /// Forward the held pointer position, if any. Everything else goes out
    /// through [`Self::send`], which flushes first, so the worker sees the
    /// same order the terminal produced.
    fn flush_hover(&mut self) {
        if let Some((x, y)) = self.hover.take() {
            self.last_hover = Instant::now();
            self.write(
                json!({"type":"mouse","action":"move","button":"left","x":x,"y":y,"dx":0,"dy":0}),
            );
        }
    }
    fn send(&mut self, value: Value) {
        self.flush_hover();
        self.write(value);
    }
    fn write(&mut self, value: Value) {
        if let Some(stdin) = &mut self.stdin {
            if writeln!(stdin, "{value}").is_err() {
                self.status = "HTML renderer disconnected — Esc to return".into();
            }
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(line) = self.frames.try_recv() {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                // Only the answer to a frame/resize request settles one: the
                // frame itself, or the error standing in for it. A stray
                // page error is not a reply.
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
                        self.status = "Live HTML · click / scroll / type · Esc or ⌘X returns · Ctrl+R reloads".into();
                    }
                }
                if let Some(error) = value["error"].as_str() {
                    self.status = error
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(200)
                        .collect();
                }
                changed = true;
            }
        }
        if let Some(child) = &mut self.child {
            if matches!(child.try_wait(), Ok(Some(_))) {
                self.status = "HTML renderer exited — Esc to return".into();
                self.stdin = None;
                self.child = None;
                self.pending = 0;
                changed = true;
            }
        }
        if self.last_hover.elapsed() >= HOVER_INTERVAL {
            self.flush_hover();
        }
        if self.stdin.is_some()
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
    pub fn draw(&mut self, f: &mut Frame) {
        let area = viewer_area(f.area());
        f.render_widget(Clear, area);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" ◇ HTML — native terminal prototype ")
                .style(
                    Style::default()
                        .bg(Color::Rgb(13, 17, 24))
                        .fg(Color::Rgb(186, 170, 235)),
                ),
            area,
        );
        f.render_widget(
            Paragraph::new(self.status.as_str()),
            Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1),
        );
        let body = Rect::new(
            area.x + 1,
            area.y + 2,
            area.width.saturating_sub(2),
            area.height.saturating_sub(3),
        );
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
            self.send(json!({"type":"resize","width":self.viewport.0,"height":self.viewport.1}));
            self.pending += 1;
            self.repaint = true;
        }
    }
    pub fn paint(&mut self, out: &mut impl Write) -> std::io::Result<()> {
        if !self.repaint {
            return Ok(());
        }
        self.repaint = false;
        let Some(png) = &self.png else {
            return Ok(());
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
    /// True closes the viewer. All other input stays out of the agent PTY.
    pub fn event(&mut self, event: Event) -> bool {
        if toggle(&event) {
            return true;
        }
        // Frames are demand-driven; an untouched document consumes no screenshots.
        if matches!(&event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_)) {
            self.needs_frame = true;
        }
        match event {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if k.code == KeyCode::Esc {
                    return true;
                }
                if k.code == KeyCode::Char('r') && k.modifiers.contains(KeyModifiers::CONTROL) {
                    self.send(json!({"type":"reload"}));
                    return false;
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
                self.send(json!({"type":"key","key":format!("{prefix}{name}")}));
            }
            Event::Paste(text) => self.send(json!({"type":"text","text":text})),
            Event::Mouse(m) if self.body.contains((m.column, m.row).into()) => {
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
                self.send(json!({"type":"mouse","action":action,"button":button,"x":x,"y":y,"dx":dx,"dy":dy}));
            }
            _ => {}
        }
        false
    }
}
impl Drop for HtmlPrototype {
    fn drop(&mut self) {
        self.send(json!({"type":"close"}));
        self.stdin.take();
        // Reap our worker after it closes its isolated browser — and kill it
        // when it doesn't, so no renderer outlives the viewer. Never block
        // TUI exit.
        if let Some(child) = self.child.take() {
            std::thread::spawn(move || reap(child, CLOSE_GRACE));
        }
        let _ = Self::clear(&mut std::io::stdout());
    }
}

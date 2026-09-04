//! Optional pane engine backed by Ghostty's VT state machine
//! (`libghostty-vt`), behind the `ghostty` cargo feature.
//!
//! Not a full replacement yet: [`AttachedTerm`] feeds the same bytes to both
//! engines, because vt100 still drives links, selection, mouse modes and
//! sizing. Only the *drawing* of the attached pane comes from Ghostty.
//! `NEBULA_GHOSTTY=0` puts drawing back on vt100 + tui-term as well.
//!
//! Drawing is our own pass over the render-state iterators rather than the
//! `ratatui-ghostty` widget, for two reasons: that crate pins libghostty-vt
//! 0.1, which links the engine as a dylib with no rpath (an installed binary
//! then dies with "Library not loaded"), and it resolves every palette color
//! to RGB. Nebula's panes must keep emitting `Color::Indexed` for palette
//! entries so the host terminal's own theme still colors agent output, the
//! way vt100 + tui-term does today.
//!
//! [`AttachedTerm`]: crate::app::AttachedTerm

use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::style::{RgbColor, Style as GStyle, StyleColor, Underline};
use libghostty_vt::terminal::{Options, ScrollViewport, Terminal};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

/// Same scrollback depth the vt100 pane keeps.
const SCROLLBACK: usize = 10_000;

/// Is the Ghostty engine on for this process? It is, unless
/// `NEBULA_GHOSTTY=0` opts back out to vt100 + tui-term.
pub fn enabled_by_env() -> bool {
    !matches!(
        std::env::var("NEBULA_GHOSTTY").as_deref(),
        Ok("0") | Ok("false") | Ok("no") | Ok("off")
    )
}

/// One pane's Ghostty terminal plus the render state its drawing needs.
pub struct GhosttyPane {
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    cols: u16,
    rows: u16,
    /// Scrollback offset in rows; 0 = live tail. Mirrors `AttachedTerm::scroll`.
    scroll: usize,
}

impl GhosttyPane {
    /// `None` if the engine refuses to allocate — the caller falls back to
    /// vt100 rather than losing the pane.
    pub fn new(cols: u16, rows: u16) -> Option<Self> {
        // libghostty-vt discards its own log messages unless a logger is
        // installed; leaving it that way keeps its "unimplemented mode"
        // warnings off the tty the alternate screen is drawn on.
        let term = Terminal::new(Options {
            cols: cols.max(1),
            rows: rows.max(1),
            max_scrollback: SCROLLBACK,
        })
        .ok()?;
        let render = RenderState::new().ok()?;
        Some(Self {
            term,
            render,
            cols: cols.max(1),
            rows: rows.max(1),
            scroll: 0,
        })
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.term.vt_write(data);
        // Output scrolls the viewport back to the tail; re-apply the offset
        // the pane is holding so scrollback stays put while a session streams.
        if self.scroll > 0 {
            self.apply_scroll();
        }
    }

    pub fn set_size(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        if self.term.resize(cols, rows, 0, 0).is_ok() {
            self.cols = cols;
            self.rows = rows;
        }
    }

    /// Rows scrolled back from the live tail; 0 = tail.
    pub fn set_scroll(&mut self, scroll: usize) {
        self.scroll = scroll;
        self.apply_scroll();
    }

    fn apply_scroll(&mut self) {
        self.term.scroll_viewport(ScrollViewport::Bottom);
        if self.scroll > 0 {
            let delta = -(self.scroll.min(isize::MAX as usize) as isize);
            self.term.scroll_viewport(ScrollViewport::Delta(delta));
        }
    }

    /// Draw the pane into `area`. The cursor is painted as a styled cell the
    /// way tui-term does it on the vt100 path — nebula never places a
    /// hardware cursor. Returns its absolute position when visible.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, focused: bool) -> Option<(u16, u16)> {
        let snapshot = self.render.update(&self.term).ok()?;
        let colors = snapshot.colors().ok()?;
        let default_fg = rgb(colors.foreground);
        let default_bg = rgb(colors.background);

        let mut rows = RowIterator::new().ok()?;
        let mut cells = CellIterator::new().ok()?;
        let mut row_iter = rows.update(&snapshot).ok()?;

        let mut y = 0u16;
        while let Some(row) = row_iter.next() {
            if y >= area.height {
                break;
            }
            if let Ok(mut cell_iter) = cells.update(row) {
                let mut x = 0u16;
                while let Some(cell) = cell_iter.next() {
                    if x >= area.width {
                        break;
                    }
                    let symbol = match cell.graphemes_len() {
                        Ok(0) | Err(_) => " ".to_string(),
                        Ok(_) => cell
                            .graphemes()
                            .map(|g| g.into_iter().collect::<String>())
                            .unwrap_or_else(|_| " ".to_string()),
                    };
                    let style = cell
                        .style()
                        .map(|s| to_ratatui(&s, default_fg, default_bg))
                        .unwrap_or_default();
                    let pos = (area.x + x, area.y + y);
                    if buf.area().contains(Position::new(pos.0, pos.1)) {
                        let target = &mut buf[pos];
                        target.set_symbol(&symbol);
                        target.set_style(style);
                    }
                    x += 1;
                }
            }
            y += 1;
        }

        if !focused || !snapshot.cursor_visible().unwrap_or(false) {
            return None;
        }
        let cursor = snapshot.cursor_viewport().ok().flatten()?;
        if cursor.at_wide_tail || cursor.x >= area.width || cursor.y >= area.height {
            return None;
        }
        let pos = (area.x + cursor.x, area.y + cursor.y);
        if buf.area().contains(Position::new(pos.0, pos.1)) {
            let modifier = match snapshot.cursor_visual_style().ok() {
                Some(CursorVisualStyle::Underline) => Modifier::UNDERLINED,
                _ => Modifier::REVERSED,
            };
            buf[pos].modifier |= modifier;
        }
        Some(pos)
    }
}

fn rgb(c: RgbColor) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

/// A cell's color, keeping palette entries as `Color::Indexed` so the host
/// terminal's theme still applies (see the module docs).
fn color(c: StyleColor) -> Option<Color> {
    match c {
        StyleColor::None => None,
        StyleColor::Rgb(c) => Some(rgb(c)),
        StyleColor::Palette(idx) => Some(Color::Indexed(idx.0 as u8)),
    }
}

fn to_ratatui(s: &GStyle, default_fg: Color, default_bg: Color) -> Style {
    let mut modifier = Modifier::empty();
    for (on, m) in [
        (s.bold, Modifier::BOLD),
        (s.italic, Modifier::ITALIC),
        (s.faint, Modifier::DIM),
        (s.blink, Modifier::SLOW_BLINK),
        (s.inverse, Modifier::REVERSED),
        (s.invisible, Modifier::HIDDEN),
        (s.strikethrough, Modifier::CROSSED_OUT),
        (
            !matches!(s.underline, Underline::None),
            Modifier::UNDERLINED,
        ),
    ] {
        if on {
            modifier |= m;
        }
    }
    Style::default()
        .fg(color(s.fg_color).unwrap_or(default_fg))
        .bg(color(s.bg_color).unwrap_or(default_bg))
        .add_modifier(modifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buf: &Buffer, area: Rect) -> Vec<String> {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn draw(pane: &mut GhosttyPane, area: Rect) -> Vec<String> {
        let mut buf = Buffer::empty(area);
        pane.render(area, &mut buf, false);
        text(&buf, area)
    }

    #[test]
    fn renders_output_with_palette_colors_left_indexed() {
        let area = Rect::new(0, 0, 30, 4);
        let mut pane = GhosttyPane::new(30, 4).unwrap();
        pane.feed(b"hello \x1b[31mred\x1b[m\r\nsecond");
        let mut buf = Buffer::empty(area);
        pane.render(area, &mut buf, false);
        let lines = text(&buf, area);
        assert_eq!(lines[0], "hello red");
        assert_eq!(lines[1], "second");
        // SGR 31 must stay palette index 1, not ghostty's RGB for it — that
        // is what keeps the host terminal's theme in charge of pane colors.
        assert_eq!(buf[(6u16, 0u16)].fg, Color::Indexed(1));
    }

    #[test]
    fn renders_truecolor_and_attributes() {
        let area = Rect::new(0, 0, 12, 1);
        let mut pane = GhosttyPane::new(12, 1).unwrap();
        pane.feed(b"\x1b[1;4;38;2;10;20;30mx");
        let mut buf = Buffer::empty(area);
        pane.render(area, &mut buf, false);
        assert_eq!(buf[(0u16, 0u16)].fg, Color::Rgb(10, 20, 30));
        assert!(buf[(0u16, 0u16)].modifier.contains(Modifier::BOLD));
        assert!(buf[(0u16, 0u16)].modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn scrollback_offset_moves_the_viewport() {
        let area = Rect::new(0, 0, 20, 3);
        let mut pane = GhosttyPane::new(20, 3).unwrap();
        for i in 0..10 {
            pane.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(draw(&mut pane, area)[0], "line 8");
        pane.set_scroll(3);
        assert_eq!(draw(&mut pane, area)[0], "line 5");
        pane.set_scroll(0);
        assert_eq!(draw(&mut pane, area)[0], "line 8");
    }

    /// Ghostty reflows on resize; vt100 truncates. This is the behaviour
    /// difference the A/B is meant to show off.
    #[test]
    fn resize_reflows() {
        let area = Rect::new(0, 0, 10, 2);
        let mut pane = GhosttyPane::new(20, 2).unwrap();
        pane.feed(b"abcdefghijklmno");
        pane.set_size(10, 2);
        let lines = draw(&mut pane, area);
        assert_eq!(lines[0], "abcdefghij");
        assert_eq!(lines[1], "klmno");
    }

    #[test]
    fn cursor_is_painted_only_when_focused() {
        let area = Rect::new(0, 0, 6, 1);
        let mut pane = GhosttyPane::new(6, 1).unwrap();
        pane.feed(b"ab");
        let mut buf = Buffer::empty(area);
        assert_eq!(pane.render(area, &mut buf, false), None);
        assert!(!buf[(2u16, 0u16)].modifier.contains(Modifier::REVERSED));
        let mut buf = Buffer::empty(area);
        assert_eq!(pane.render(area, &mut buf, true), Some((2, 0)));
        assert!(buf[(2u16, 0u16)].modifier.contains(Modifier::REVERSED));
    }
}

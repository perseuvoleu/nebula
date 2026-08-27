//! Plain-text rendering of a session's output ring for `ReadSession`
//! (`nebula agent read`): feed the raw bytes through vt100 at the session's
//! live grid size and dump scrollback + screen as text. Rendering here —
//! instead of replaying the ring to the client — keeps the read side-effect
//! free: no attach, no resize jiggle, no respawn of a dead session.

/// Rows of scrollback the render parser retains — matches the TUI's parser,
/// and comfortably exceeds what a 1MB byte ring can hold in practice.
const SCROLLBACK_ROWS: usize = 10_000;

/// Render `data` (the ring's bytes) on a cols×rows grid and return the
/// retained scrollback plus the visible screen as one plain-text block,
/// trailing blank lines trimmed. `lines` keeps only that many final lines.
pub fn ring_to_text(data: &[u8], cols: u16, rows: u16, lines: Option<usize>) -> String {
    let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
    parser.process(data);
    let screen = parser.screen_mut();

    // How much scrollback the grid actually kept (0 on the alternate
    // screen, where full-screen CLIs live).
    screen.set_scrollback(usize::MAX);
    let retained = screen.scrollback();

    // Walk the scrollback in screen-height windows, oldest first. Window at
    // offset `o` shows logical rows [retained - o, retained - o + rows); the
    // final step to offset 0 may overlap the previous window, so rows are
    // slotted by logical index and never overwritten.
    let total = retained + rows as usize;
    let mut all: Vec<Option<String>> = vec![None; total];
    let mut offset = retained;
    loop {
        screen.set_scrollback(offset);
        for (i, row) in screen.rows(0, cols).enumerate() {
            let idx = retained - offset + i;
            if let Some(slot) = all.get_mut(idx) {
                if slot.is_none() {
                    *slot = Some(row);
                }
            }
        }
        if offset == 0 {
            break;
        }
        offset = offset.saturating_sub(rows as usize);
    }

    let mut text: Vec<String> = all.into_iter().map(Option::unwrap_or_default).collect();
    while text.last().is_some_and(|l| l.trim().is_empty()) {
        text.pop();
    }
    if let Some(keep) = lines {
        if text.len() > keep {
            text.drain(..text.len() - keep);
        }
    }
    text.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_screen_text() {
        assert_eq!(ring_to_text(b"hello\r\nworld", 20, 5, None), "hello\nworld");
    }

    #[test]
    fn includes_scrolled_out_rows_and_tails() {
        let mut data = Vec::new();
        for n in 1..=10 {
            data.extend_from_slice(format!("line{n}\r\n").as_bytes());
        }
        let all = ring_to_text(&data, 20, 3, None);
        assert!(all.starts_with("line1\n"), "scrollback missing: {all:?}");
        assert!(all.ends_with("line10"), "screen tail missing: {all:?}");
        assert_eq!(ring_to_text(&data, 20, 3, Some(2)), "line9\nline10");
    }
}

//! Tail-reads a Claude Code session transcript for the sessions panel's
//! last-message preview.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// How much of the transcript's tail to scan. Assistant turns are a few
/// KB each, so the last one sits comfortably inside this window.
const TAIL_BYTES: u64 = 64 * 1024;

/// Claude Code's directory slug for a session cwd: the absolute path with
/// every non-alphanumeric character flattened to `-` (verified against
/// `~/.claude/projects/`, where `/` and `.` both arrive as `-`).
fn slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Where Claude Code writes the transcript for a session running in `cwd`:
/// `~/.claude/projects/<slug>/<session_id>.jsonl`.
pub fn transcript_path(cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(
        home.join(".claude/projects")
            .join(slug(cwd))
            .join(format!("{session_id}.jsonl")),
    )
}

/// The last assistant text in the transcript at `path`, whitespace
/// collapsed, untruncated. Only the file's tail is read; a mid-file start
/// drops the partial line it lands in. None when the file is missing,
/// unreadable, or holds no assistant text inside the window.
pub fn last_assistant_text(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let tail = String::from_utf8_lossy(&bytes);
    let tail = match start {
        0 => tail.as_ref(),
        // Everything before the first newline is the tail of a line whose
        // head fell outside the window.
        _ => tail.split_once('\n').map_or("", |(_, rest)| rest),
    };
    for line in tail.lines().rev() {
        // A partial trailing line (the CLI mid-write) fails to parse and
        // is skipped, same as any non-JSON noise.
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
            continue;
        };
        let text = blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ");
        // Thinking-only turns carry no text; keep scanning for one that does.
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn assistant_line(texts: &[&str]) -> String {
        let blocks: Vec<serde_json::Value> = std::iter::once(
            serde_json::json!({"type": "thinking", "thinking": "hm"}),
        )
        .chain(
            texts
                .iter()
                .map(|t| serde_json::json!({"type": "text", "text": t})),
        )
        .collect();
        serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": blocks},
        })
        .to_string()
    }

    #[test]
    fn last_assistant_text_reads_the_newest_text_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"content":"do it"}}}}"#).unwrap();
        writeln!(f, "{}", assistant_line(&["old answer"])).unwrap();
        writeln!(f, "{}", assistant_line(&["done:  tests", "\n all\tgreen "])).unwrap();
        // Thinking-only turn after it, then a partial trailing line.
        writeln!(f, "{}", assistant_line(&[])).unwrap();
        write!(f, r#"{{"type":"assistant","mess"#).unwrap();
        assert_eq!(
            last_assistant_text(&path).as_deref(),
            Some("done: tests all green"),
            "multi-block text joined, whitespace collapsed"
        );
    }

    #[test]
    fn last_assistant_text_survives_a_mid_line_window_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", assistant_line(&["before the window"])).unwrap();
        // One line larger than the tail window, so the scan starts inside
        // it and must drop the fragment.
        let filler = serde_json::json!({
            "type": "user",
            "message": {"content": "x".repeat(TAIL_BYTES as usize)},
        });
        writeln!(f, "{filler}").unwrap();
        writeln!(f, "{}", assistant_line(&["after"])).unwrap();
        assert_eq!(last_assistant_text(&path).as_deref(), Some("after"));
    }

    #[test]
    fn missing_or_empty_files_yield_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(last_assistant_text(&dir.path().join("gone.jsonl")), None);
        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(last_assistant_text(&empty), None);
    }

    #[test]
    fn slug_flattens_every_non_alphanumeric_to_a_dash() {
        assert_eq!(
            slug(Path::new("/Users/x/Desktop/nebula")),
            "-Users-x-Desktop-nebula"
        );
        assert_eq!(slug(Path::new("/a/.her_dr v2")), "-a--her-dr-v2");
    }
}

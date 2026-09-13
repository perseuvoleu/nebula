//! Path viewer (⌘I): a path box on top, the file's contents below. The
//! box takes anything a shell would — a path relative to the worktree,
//! an absolute one, `~/…` — and reads straight from disk, so ignored and
//! untracked files (`.scratch/…`) show too, unlike the git-listing modals.
//!
//! Until the text names a file, the pane is a fuzzy finder over every
//! file of the worktree (tracked, untracked AND ignored — minus build
//! dirs, see `SKIP_DIRS`): ↑/↓ walk the matches, Tab/Enter picks one and
//! shows it. Once a file is shown the same keys scroll its contents.

use crate::syntax::{Highlighter, TokenKind};
use crate::text_input::TextInput;
use ratatui::layout::Rect;
use std::path::{Path, PathBuf};

/// What the content pane is showing for the current path text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileViewMode {
    /// A real file: `lines` are its contents.
    File,
    /// Fuzzy-finding: `candidates` are the matching worktree files.
    Candidates,
    /// Nothing matches: `lines` carry the message.
    Missing,
}

#[derive(Debug, Clone)]
pub struct FileView {
    /// Checkout dir relative paths resolve against.
    pub root: PathBuf,
    /// Branch name for the modal title.
    pub branch: String,
    /// The path box.
    pub path: TextInput,
    pub mode: FileViewMode,
    /// The file on screen (File mode), for the title and Ctrl+y.
    pub shown: Option<PathBuf>,
    /// Every file under the root, relative, in listing order.
    pub files: Vec<String>,
    /// Fuzzy matches over `files` for the typed text: index into `files`
    /// plus matched char positions, best first.
    pub candidates: Vec<(usize, Vec<usize>)>,
    /// Index into `candidates`.
    pub selected: usize,
    /// Highlighted content lines (File mode) or the message (Missing).
    pub lines: Vec<Vec<(TokenKind, String)>>,
    pub scroll: u16,
    /// Content pane height, written back during draw for paging.
    pub view_height: u16,
    /// Whole modal rect, written back during draw so clicks outside close.
    pub area: Rect,
    /// Screen rect of the content rows, written back during draw.
    pub content_area: Rect,
}

impl FileView {
    pub fn new(root: PathBuf, branch: String) -> Self {
        let files = list_worktree_files(&root);
        let mut view = Self {
            root,
            branch,
            path: TextInput::new(),
            mode: FileViewMode::Candidates,
            shown: None,
            files,
            candidates: Vec::new(),
            selected: 0,
            lines: Vec::new(),
            scroll: 0,
            view_height: 0,
            area: Rect::default(),
            content_area: Rect::default(),
        };
        view.refresh();
        view
    }

    /// The typed text as a filesystem path: `~` expands, absolute stays,
    /// anything else hangs off the worktree root.
    pub fn resolve(&self, text: &str) -> PathBuf {
        let text = text.trim();
        if let Some(rest) = text
            .strip_prefix("~/")
            .or_else(|| (text == "~").then_some(""))
        {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
        let p = Path::new(text);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    /// Re-read what the path box names. Called after every edit.
    pub fn refresh(&mut self) {
        self.scroll = 0;
        self.selected = 0;
        self.shown = None;
        self.candidates.clear();
        self.lines.clear();
        let text = self.path.as_str().to_string();
        let full = self.resolve(&text);
        if full.is_file() {
            self.mode = FileViewMode::File;
            let (body, highlight) = match crate::tree_browser::read_preview(&full) {
                Ok(body) => (body, true),
                Err(msg) => (msg, false),
            };
            let mut hl = if highlight {
                Highlighter::for_path(&full.to_string_lossy())
            } else {
                Highlighter::plain()
            };
            self.lines = body.lines().map(|l| hl.line(l)).collect();
            self.shown = Some(full);
            return;
        }
        // Not a file: fuzzy-find over the worktree listing. Absolute and
        // `~` paths are outside the listing, so they only resolve exactly.
        let query = text.trim();
        let outside = query.starts_with('/') || query.starts_with('~');
        self.candidates = if outside {
            Vec::new()
        } else {
            crate::fuzzy::rank(query, self.files.iter().map(String::as_str))
        };
        if self.candidates.is_empty() {
            self.mode = FileViewMode::Missing;
            let mut hl = Highlighter::plain();
            self.lines = vec![hl.line(&format!("no such file: {}", full.display()))];
        } else {
            self.mode = FileViewMode::Candidates;
        }
    }

    /// The candidate row's path.
    pub fn candidate_path(&self, i: usize) -> Option<&str> {
        self.candidates
            .get(i)
            .and_then(|(f, _)| self.files.get(*f))
            .map(String::as_str)
    }

    /// Tab / Enter on a candidate: the path box takes its path and the
    /// file shows.
    pub fn complete(&mut self) {
        if self.mode != FileViewMode::Candidates {
            return;
        }
        let Some(pick) = self.candidate_path(self.selected).map(str::to_string) else {
            return;
        };
        self.path.set_text(pick);
        self.refresh();
    }

    pub fn line_count(&self) -> usize {
        match self.mode {
            FileViewMode::Candidates => self.candidates.len(),
            _ => self.lines.len(),
        }
    }

    pub fn max_scroll(&self) -> u16 {
        (self.line_count() as u16).saturating_sub(self.view_height.max(1))
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.max_scroll() as i32;
        self.scroll = (self.scroll as i32 + delta).clamp(0, max) as u16;
    }

    /// ↑/↓: walk the candidates, or scroll the file.
    pub fn step(&mut self, delta: i32) {
        match self.mode {
            FileViewMode::Candidates => {
                let n = self.candidates.len();
                if n == 0 {
                    return;
                }
                self.selected = (self.selected as i64 + delta as i64).rem_euclid(n as i64) as usize;
                // Keep the highlighted row on screen.
                let h = self.view_height.max(1) as usize;
                if self.selected < self.scroll as usize {
                    self.scroll = self.selected as u16;
                } else if self.selected >= self.scroll as usize + h {
                    self.scroll = (self.selected + 1 - h) as u16;
                }
            }
            _ => self.scroll_by(delta),
        }
    }

    /// The path a click on candidate row `row` (screen row within the
    /// content pane) completes to, if any.
    pub fn candidate_at_row(&self, row: usize) -> Option<usize> {
        let i = self.scroll as usize + row;
        (self.mode == FileViewMode::Candidates && i < self.candidates.len()).then_some(i)
    }
}

/// Directories never worth fuzzy-finding into: build output and vendored
/// dependency trees, which dwarf the source and are ignored anyway.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    ".turbo",
    "vendor",
];
/// Hard cap on the listing, so a pathological checkout stays snappy.
const MAX_FILES: usize = 100_000;

/// Every file under `root` (relative, `/`-separated), tracked or not,
/// ignored or not — a plain walk, since the point of this modal is
/// reaching `.scratch/…`-style files git's listings leave out. Sorted so
/// the empty-query view is a stable tree order.
pub fn list_worktree_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if ft.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                    if out.len() >= MAX_FILES {
                        out.sort();
                        return out;
                    }
                }
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, FileView) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".scratch")).unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join(".scratch/notes.md"), "# hi\nline two\n").unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let view = FileView::new(tmp.path().to_path_buf(), "main".into());
        (tmp, view)
    }

    fn candidate_paths(view: &FileView) -> Vec<String> {
        (0..view.candidates.len())
            .filter_map(|i| view.candidate_path(i).map(str::to_string))
            .collect()
    }

    #[test]
    fn empty_query_lists_every_file_including_dotdirs() {
        let (_tmp, view) = fixture();
        assert_eq!(view.mode, FileViewMode::Candidates);
        assert_eq!(
            candidate_paths(&view),
            vec![".scratch/notes.md".to_string(), "src/main.rs".to_string()]
        );
    }

    #[test]
    fn full_relative_path_shows_the_file() {
        let (tmp, mut view) = fixture();
        view.path.set_text(".scratch/notes.md");
        view.refresh();
        assert_eq!(view.mode, FileViewMode::File);
        assert_eq!(view.lines.len(), 2);
        assert_eq!(view.shown, Some(tmp.path().join(".scratch/notes.md")));
    }

    #[test]
    fn fuzzy_query_ranks_and_tab_picks() {
        let (_tmp, mut view) = fixture();
        view.path.set_text("scrnot");
        view.refresh();
        assert_eq!(
            candidate_paths(&view),
            vec![".scratch/notes.md".to_string()]
        );
        view.complete();
        assert_eq!(view.path.as_str(), ".scratch/notes.md");
        assert_eq!(view.mode, FileViewMode::File);
    }

    #[test]
    fn missing_path_says_so() {
        let (_tmp, mut view) = fixture();
        view.path.set_text("zzqqx");
        view.refresh();
        assert_eq!(view.mode, FileViewMode::Missing);
    }

    #[test]
    fn listing_skips_build_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("target/debug")).unwrap();
        std::fs::write(tmp.path().join("target/debug/bin"), "x").unwrap();
        std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
        assert_eq!(list_worktree_files(tmp.path()), vec!["a.txt".to_string()]);
    }
}

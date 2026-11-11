//! `git grep` runner for the find-in-files overlay.
//!
//! Synchronous `std::process` on purpose (the git_diff.rs precedent): a
//! search runs only on key events, and `git grep` over a checkout is fast.

use crate::git_diff::Source;
use std::process::Command;

/// Queries shorter than this don't search — one character would light up
//  half the repo.
pub const MIN_QUERY_LEN: usize = 2;
/// Hits beyond this are dropped (`truncated` tells the UI to say so).
pub const MAX_RESULTS: usize = 200;
/// Matched lines are clipped to this many chars to keep overlay state small.
const MAX_TEXT_LEN: usize = 250;

/// One `git grep` match.
#[derive(Debug, Clone, PartialEq)]
pub struct GrepHit {
    /// Path relative to the search root.
    pub path: String,
    /// 1-based line number.
    pub line: u64,
    /// The matched line, trimmed and clipped.
    pub text: String,
}

/// A typed query: `word word  path path`. Every word must appear on a
/// matched line; text after the first two consecutive spaces is a path
/// filter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GrepQuery {
    pub words: Vec<String>,
    pub paths: Vec<String>,
}

impl GrepQuery {
    pub fn parse(text: &str) -> Self {
        let (words, paths) = text.split_once("  ").unwrap_or((text, ""));
        let split = |s: &str| s.split_whitespace().map(str::to_string).collect();
        Self {
            words: split(words),
            paths: split(paths),
        }
    }

    /// Smart case over the whole query: literal case only when a word has
    /// an uppercase char.
    pub fn case_sensitive(&self) -> bool {
        self.words.iter().any(|w| w.chars().any(char::is_uppercase))
    }

    /// The path filter as git pathspecs: a word with `*` is a glob (at any
    /// depth unless it names a directory), anything else a path prefix.
    pub fn pathspecs(&self) -> Vec<String> {
        self.paths
            .iter()
            .map(|p| match (p.contains('*'), p.contains('/')) {
                (false, _) => p.clone(),
                (true, false) => format!(":(glob)**/{p}"),
                (true, true) => format!(":(glob){p}"),
            })
            .collect()
    }

    /// Sorted char positions in `line` covered by any word, for
    /// highlighting.
    pub fn match_positions(&self, line: &str) -> Vec<usize> {
        let sensitive = self.case_sensitive();
        let fold = |c: char| -> char {
            if sensitive {
                c
            } else {
                c.to_lowercase().next().unwrap_or(c)
            }
        };
        let hay: Vec<char> = line.chars().map(fold).collect();
        let mut lit = vec![false; hay.len()];
        for word in &self.words {
            let needle: Vec<char> = word.chars().map(fold).collect();
            if needle.is_empty() || needle.len() > hay.len() {
                continue;
            }
            for start in 0..=hay.len() - needle.len() {
                if hay[start..start + needle.len()] == needle[..] {
                    lit[start..start + needle.len()].fill(true);
                }
            }
        }
        (0..hay.len()).filter(|&i| lit[i]).collect()
    }
}

/// Fixed-string smart-case search over the source: a checkout's tracked +
/// untracked files (gitignore respected), or a revision's tree; binaries
/// skipped. `query` follows `GrepQuery::parse`. Returns `(hits,
/// truncated)`; `Err` is a user-facing message shown inside the overlay.
pub fn search(source: &Source, query: &str) -> Result<(Vec<GrepHit>, bool), String> {
    let query = GrepQuery::parse(query);
    if query.words.is_empty() {
        return Ok((Vec::new(), false));
    }
    let mut args = vec!["grep", "-z", "-n", "-I", "--no-color", "-F"];
    if source.has_working_tree() {
        args.push("--untracked");
    }
    if !query.case_sensitive() {
        args.push("-i");
    }
    for (i, word) in query.words.iter().enumerate() {
        if i > 0 {
            args.push("--and");
        }
        args.extend(["-e", word]);
    }
    args.extend(source.rev());
    args.push("--");
    let pathspecs = query.pathspecs();
    if pathspecs.is_empty() {
        args.push(".");
    }
    args.extend(pathspecs.iter().map(String::as_str));
    // Over ssh for a remote checkout — same argv, other machine.
    let (program, argv) = nebula_core::remote::git_command(source.dir(), &args);
    let output = Command::new(program)
        .args(argv)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    // git grep exits 1 for "no matches" — only >= 2 is an error.
    match output.status.code() {
        Some(0) => {
            let (mut hits, truncated) = parse_grep_z(&output.stdout);
            if let Some(rev) = source.rev() {
                let prefix = format!("{rev}:");
                for hit in &mut hits {
                    if let Some(path) = hit.path.strip_prefix(&prefix) {
                        hit.path = path.to_string();
                    }
                }
            }
            Ok((hits, truncated))
        }
        Some(1) => Ok((Vec::new(), false)),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("git grep failed: {}", stderr.trim()))
        }
    }
}

/// Parse `git grep -z -n` records: `<path>\0<line>\0<text>\n`, stopping at
/// `MAX_RESULTS` (the bool says whether anything was dropped).
pub fn parse_grep_z(bytes: &[u8]) -> (Vec<GrepHit>, bool) {
    let mut hits = Vec::new();
    for record in bytes.split(|b| *b == b'\n') {
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, |b| *b == 0);
        let (Some(path), Some(line), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(line) = String::from_utf8_lossy(line).parse::<u64>() else {
            continue;
        };
        if hits.len() >= MAX_RESULTS {
            return (hits, true);
        }
        let text = String::from_utf8_lossy(text);
        let text = text.trim();
        hits.push(GrepHit {
            path: String::from_utf8_lossy(path).into_owned(),
            line,
            text: clip(text, MAX_TEXT_LEN),
        });
    }
    (hits, false)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_grep_z_splits_path_line_text() {
        let bytes = b"src/a.rs\x007\x00    let x = grep_me();\nsrc/b file.rs\x0012\x00grep_me\n";
        let (hits, truncated) = parse_grep_z(bytes);
        assert!(!truncated);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "src/a.rs");
        assert_eq!(hits[0].line, 7);
        assert_eq!(hits[0].text, "let x = grep_me();", "text is trimmed");
        assert_eq!(hits[1].path, "src/b file.rs", "spaces in paths survive");
        assert_eq!(hits[1].line, 12);
    }

    #[test]
    fn parse_grep_z_caps_results() {
        let mut bytes = Vec::new();
        for i in 0..(MAX_RESULTS + 5) {
            bytes.extend_from_slice(format!("f.rs\x00{}\x00match\n", i + 1).as_bytes());
        }
        let (hits, truncated) = parse_grep_z(&bytes);
        assert_eq!(hits.len(), MAX_RESULTS);
        assert!(truncated);
    }

    fn git(repo: &PathBuf, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn make_repo(dir: &tempfile::TempDir) -> PathBuf {
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("tracked.txt"), "needle in a haystack\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);
        repo
    }

    #[test]
    fn search_finds_tracked_and_untracked() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        std::fs::write(repo.join("fresh.txt"), "another Needle here\n").unwrap();

        // Lowercase query is case-insensitive (smart case): both files hit.
        let (hits, truncated) = search(&Source::Checkout(repo.clone()), "needle").unwrap();
        assert!(!truncated);
        assert_eq!(hits.len(), 2, "{hits:?}");
        let tracked = hits.iter().find(|h| h.path == "tracked.txt").unwrap();
        assert_eq!(tracked.line, 1);
        assert_eq!(tracked.text, "needle in a haystack");
        assert!(hits.iter().any(|h| h.path == "fresh.txt"), "{hits:?}");

        // An uppercase char makes the query literal: only fresh.txt hits.
        let (hits, _) = search(&Source::Checkout(repo.clone()), "Needle").unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].path, "fresh.txt");

        // No matches is Ok(empty), not an error (exit code 1).
        let (hits, _) = search(&Source::Checkout(repo.clone()), "no_such_string_zzz").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn every_word_must_appear_on_the_line_in_any_order() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        std::fs::write(repo.join("both.txt"), "beta comes before alpha\n").unwrap();
        std::fs::write(repo.join("only.txt"), "alpha alone\n").unwrap();

        let (hits, _) = search(&Source::Checkout(repo.clone()), "alpha beta").unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["both.txt"]);
    }

    #[test]
    fn text_after_two_spaces_filters_by_glob_or_path_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        std::fs::create_dir(repo.join("sub")).unwrap();
        for file in ["top.rs", "top.txt", "sub/deep.rs", "sub/deep.txt"] {
            std::fs::write(repo.join(file), "alpha\n").unwrap();
        }
        let paths = |query: &str| -> Vec<String> {
            let (hits, _) = search(&Source::Checkout(repo.clone()), query).unwrap();
            hits.into_iter().map(|h| h.path).collect()
        };

        assert_eq!(paths("alpha  *.rs"), vec!["sub/deep.rs", "top.rs"]);
        assert_eq!(paths("alpha  sub/"), vec!["sub/deep.rs", "sub/deep.txt"]);
        assert_eq!(
            paths("alpha  sub/ *.rs"),
            vec!["sub/deep.rs", "sub/deep.txt", "top.rs"],
            "several pathspecs widen the filter"
        );
    }

    #[test]
    fn query_parses_words_and_path_filter() {
        let q = GrepQuery::parse("foo  bar  *.rs crates/x");
        assert_eq!(q.words, vec!["foo"]);
        assert_eq!(q.paths, vec!["bar", "*.rs", "crates/x"]);
        assert_eq!(
            q.pathspecs(),
            vec!["bar", ":(glob)**/*.rs", "crates/x"],
            "a bare glob matches at any depth"
        );
        assert_eq!(GrepQuery::parse(" foo bar ").words, vec!["foo", "bar"]);
        assert!(GrepQuery::parse("foo bar").paths.is_empty());
        let filter_only = GrepQuery::parse("  *.rs");
        assert!(filter_only.words.is_empty(), "{filter_only:?}");
    }

    #[test]
    fn match_positions_light_every_word_with_smart_case() {
        let q = GrepQuery::parse("ab cd");
        assert_eq!(q.match_positions("xCD ab"), vec![1, 2, 4, 5]);
        let q = GrepQuery::parse("Ab");
        assert_eq!(
            q.match_positions("ab Ab"),
            vec![3, 4],
            "uppercase is literal"
        );
        let q = GrepQuery::parse("é");
        assert_eq!(
            q.match_positions("aÉé"),
            vec![1, 2],
            "positions count chars"
        );
    }

    #[test]
    fn search_errors_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = search(&Source::Checkout(dir.path().to_path_buf()), "anything").unwrap_err();
        assert!(err.contains("git grep failed"), "{err}");
    }
}

//! Bash-style filesystem path completion for the Add-project prompt.
//!
//! Semantics (matching bash's readline closely enough to feel familiar):
//! - single match → complete it fully and append `/`
//! - multiple matches → extend the input to the longest common prefix and
//!   surface the candidate list for display
//! - `~/…` is expanded for lookup but the tilde is preserved in the input
//! - only directories are offered (projects are git repos)
//! - dotted entries are hidden unless the partial segment starts with `.`

use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq)]
pub struct PathCompletion {
    /// New input value when the completion made progress.
    pub completed: Option<String>,
    /// Candidate directory names (with trailing `/`) when ambiguous.
    pub candidates: Vec<String>,
}

/// One row of the Add-project directory listing.
#[derive(Debug, Clone, PartialEq)]
pub struct DirEntry {
    /// Directory name, no trailing slash.
    pub name: String,
    /// Whether it holds a `.git` entry (dir, or file for linked worktrees).
    pub is_repo: bool,
}

/// Split `input` at its last '/': the typed parent (kept verbatim, tilde
/// and all) and the partial segment being completed.
pub fn split_input(input: &str) -> (&str, &str) {
    match input.rfind('/') {
        Some(idx) => (&input[..=idx], &input[idx + 1..]),
        None => ("", input),
    }
}

/// Live listing behind the Add-project browser: the typed parent's
/// subdirectories narrowed to the partial segment — fuzzily, so "wrk"
/// still finds "my-work" — best matches first. Same visibility rules as
/// [`complete_path`] (directories only, dotted entries need a dotted
/// partial), plus a git-repo marker per entry. Tab completion stays
/// bash-prefix; only this listing is fuzzy.
pub fn list_dirs(input: &str, home: Option<&Path>) -> Vec<DirEntry> {
    let (typed_parent, partial) = split_input(input);
    let parent_dir = expand_parent(typed_parent, home);
    let Ok(entries) = std::fs::read_dir(&parent_dir) else {
        return Vec::new();
    };
    let show_hidden = partial.starts_with('.');
    let mut dirs: Vec<(u8, DirEntry)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| show_hidden || !name.starts_with('.'))
        .filter_map(|name| {
            let rank = fuzzy_rank(&name, partial)?;
            let is_repo = parent_dir.join(&name).join(".git").exists();
            Some((rank, DirEntry { name, is_repo }))
        })
        .collect();
    dirs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    dirs.into_iter().map(|(_, d)| d).collect()
}

/// How deep [`scan_parent`] descends below the typed parent.
const DEEP_SCAN_DEPTH: usize = 3;
/// Directories visited before a scan gives up (keeps a keystroke snappy
/// even under a huge parent — the cache makes it a one-time cost anyway).
const DEEP_SCAN_BUDGET: usize = 25_000;
/// Build/system dirs that are never worth descending into.
const DEEP_SKIP: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "Library",
    "Applications",
    "Movies",
    "Music",
    "Pictures",
];

/// Recursive directory scan behind the Add-project fuzzy finder: every
/// directory up to [`DEEP_SCAN_DEPTH`] below `input`'s typed parent, as
/// parent-relative paths ("Desktop/nebula"). Hidden and [`DEEP_SKIP`] dirs
/// are pruned, and a git repo is a leaf — its insides are never projects.
/// The caller caches the result per parent; [`filter_deep`] narrows it per
/// keystroke.
pub fn scan_parent(input: &str, home: Option<&Path>) -> Vec<DirEntry> {
    let (typed_parent, _) = split_input(input);
    let root = expand_parent(typed_parent, home);
    let mut out = Vec::new();
    let mut visited = 0usize;
    let mut stack = vec![(root, String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            if visited >= DEEP_SCAN_BUDGET {
                return out;
            }
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') || DEEP_SKIP.contains(&name.as_str()) {
                continue;
            }
            visited += 1;
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let is_repo = e.path().join(".git").exists();
            if !is_repo && depth + 1 < DEEP_SCAN_DEPTH {
                stack.push((e.path(), rel.clone(), depth + 1));
            }
            out.push(DirEntry { name: rel, is_repo });
        }
    }
    out
}

/// Narrow a [`scan_parent`] listing to `partial`, best matches first:
/// basename match quality, then repos before plain dirs, then shallow
/// before deep, then name order. A path-only match (the basename itself
/// misses) still lists, one rank down. Whitespace splits the query into
/// tokens that must all match somewhere — "nebula desktop" finds
/// Desktop/nebula in either word order.
pub fn filter_deep(scanned: &[DirEntry], partial: &str) -> Vec<DirEntry> {
    let tokens: Vec<&str> = partial.split_whitespace().collect();
    let mut hits: Vec<(u8, usize, &DirEntry)> = scanned
        .iter()
        .filter_map(|d| {
            let base = d.name.rsplit('/').next().unwrap_or(&d.name);
            let mut total = 0u8;
            for token in &tokens {
                let rank = fuzzy_rank(base, token)
                    .or_else(|| fuzzy_rank(&d.name, token).map(|r| r.saturating_add(3)))?;
                total = total.saturating_add(rank);
            }
            Some((total, d.name.matches('/').count(), d))
        })
        .collect();
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.2.is_repo.cmp(&a.2.is_repo))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.name.cmp(&b.2.name))
    });
    hits.truncate(500);
    hits.into_iter().map(|(_, _, d)| d.clone()).collect()
}

/// Character positions of `partial`'s match inside `name`, for the
/// listing highlight — the union over the query's whitespace-separated
/// tokens; a token that misses (matched via the full path, or cut off by
/// truncation) just contributes nothing.
pub fn match_positions(name: &str, partial: &str) -> Vec<usize> {
    let mut all: Vec<usize> = partial
        .split_whitespace()
        .flat_map(|token| token_positions(name, token))
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// One token's positions: substring positions when `name` contains it,
/// greedy subsequence positions otherwise; empty when it misses.
fn token_positions(name: &str, partial: &str) -> Vec<usize> {
    if partial.is_empty() {
        return Vec::new();
    }
    let name_l = name.to_lowercase();
    let partial_l = partial.to_lowercase();
    // Byte offset → char index only works when the lowercase mapping is
    // 1:1 per char; fall back to the subsequence walk otherwise.
    if name_l.chars().count() == name.chars().count() {
        if let Some(byte) = name_l.find(&partial_l) {
            let start = name_l[..byte].chars().count();
            return (start..start + partial_l.chars().count()).collect();
        }
    }
    let mut positions = Vec::new();
    let mut chars = name_l.chars().enumerate();
    for c in partial_l.chars() {
        match chars.find(|(_, n)| *n == c) {
            Some((i, _)) => positions.push(i),
            None => return Vec::new(),
        }
    }
    positions
}

/// Match quality of `partial` against `name`, case-insensitive: 0 prefix,
/// 1 substring, 2 subsequence ("nbl" hits "nebula"); None = no match.
fn fuzzy_rank(name: &str, partial: &str) -> Option<u8> {
    if partial.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    let partial = partial.to_lowercase();
    if name.starts_with(&partial) {
        return Some(0);
    }
    if name.contains(&partial) {
        return Some(1);
    }
    let mut rest = name.chars();
    partial.chars().all(|c| rest.any(|n| n == c)).then_some(2)
}

/// Complete `input` against the filesystem. `home` backs `~` expansion.
pub fn complete_path(input: &str, home: Option<&Path>) -> PathCompletion {
    // Bare "~" → "~/" so the next Tab lists the home directory.
    if input == "~" {
        return PathCompletion {
            completed: Some("~/".into()),
            candidates: vec![],
        };
    }

    let (typed_parent, partial) = split_input(input);

    let parent_dir = expand_parent(typed_parent, home);
    let Ok(entries) = std::fs::read_dir(&parent_dir) else {
        return PathCompletion::default();
    };

    let show_hidden = partial.starts_with('.');
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with(partial))
        .filter(|name| show_hidden || !name.starts_with('.'))
        .collect();
    names.sort();

    match names.len() {
        0 => PathCompletion::default(),
        1 => PathCompletion {
            completed: Some(format!("{typed_parent}{}/", names[0])),
            candidates: vec![],
        },
        _ => {
            let lcp = longest_common_prefix(&names);
            let completed = if lcp.len() > partial.len() {
                Some(format!("{typed_parent}{lcp}"))
            } else {
                None
            };
            PathCompletion {
                completed,
                candidates: names.into_iter().map(|n| format!("{n}/")).collect(),
            }
        }
    }
}

/// Expand the typed parent ("", "~/", "~/x/", "/a/b/", "rel/") to a real dir.
fn expand_parent(typed_parent: &str, home: Option<&Path>) -> PathBuf {
    if typed_parent.is_empty() {
        return PathBuf::from(".");
    }
    if let Some(home) = home {
        if typed_parent == "~/" {
            return home.to_path_buf();
        }
        if let Some(rest) = typed_parent.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(typed_parent)
}

fn longest_common_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut lcp = first.as_str();
    for name in &names[1..] {
        let common_bytes = lcp
            .char_indices()
            .zip(name.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map(|((i, c), _)| i + c.len_utf8())
            .unwrap_or(0);
        lcp = &lcp[..common_bytes];
        if lcp.is_empty() {
            break;
        }
    }
    lcp.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// projects/{alpha,alps,beta,.hidden}/ + a file notes.txt
    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for dir in [
            "projects/alpha",
            "projects/alps",
            "projects/beta",
            "projects/.hidden",
        ] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        std::fs::write(tmp.path().join("projects/notes.txt"), "").unwrap();
        tmp
    }

    fn p(tmp: &tempfile::TempDir, rest: &str) -> String {
        format!("{}/{rest}", tmp.path().display())
    }

    #[test]
    fn single_match_completes_with_trailing_slash() {
        let tmp = fixture();
        let got = complete_path(&p(&tmp, "proj"), None);
        assert_eq!(got.completed, Some(p(&tmp, "projects/")));
        assert!(got.candidates.is_empty());
    }

    #[test]
    fn unambiguous_nested_match() {
        let tmp = fixture();
        let got = complete_path(&p(&tmp, "projects/be"), None);
        assert_eq!(got.completed, Some(p(&tmp, "projects/beta/")));
    }

    #[test]
    fn ambiguous_extends_to_longest_common_prefix_and_lists() {
        let tmp = fixture();
        let got = complete_path(&p(&tmp, "projects/a"), None);
        assert_eq!(
            got.completed,
            Some(p(&tmp, "projects/alp")),
            "extends a → alp"
        );
        assert_eq!(got.candidates, vec!["alpha/", "alps/"]);
    }

    #[test]
    fn at_lcp_already_just_lists_candidates() {
        let tmp = fixture();
        let got = complete_path(&p(&tmp, "projects/alp"), None);
        assert_eq!(got.completed, None, "no further progress possible");
        assert_eq!(got.candidates, vec!["alpha/", "alps/"]);
    }

    #[test]
    fn files_are_not_offered() {
        let tmp = fixture();
        let got = complete_path(&p(&tmp, "projects/no"), None);
        assert_eq!(
            got,
            PathCompletion::default(),
            "notes.txt is a file, not a project dir"
        );
    }

    #[test]
    fn hidden_dirs_only_with_dot_partial() {
        let tmp = fixture();
        // "projects/" partial "" → hidden excluded, both visible prefixes listed.
        let got = complete_path(&p(&tmp, "projects/"), None);
        assert_eq!(got.candidates, vec!["alpha/", "alps/", "beta/"]);
        // Typing the dot opts in.
        let got = complete_path(&p(&tmp, "projects/."), None);
        assert_eq!(got.completed, Some(p(&tmp, "projects/.hidden/")));
    }

    #[test]
    fn tilde_is_expanded_for_lookup_but_preserved_in_input() {
        let tmp = fixture();
        let got = complete_path("~/proj", Some(tmp.path()));
        assert_eq!(got.completed, Some("~/projects/".into()));
        let got = complete_path("~/projects/be", Some(tmp.path()));
        assert_eq!(got.completed, Some("~/projects/beta/".into()));
    }

    #[test]
    fn bare_tilde_becomes_tilde_slash() {
        let got = complete_path("~", None);
        assert_eq!(got.completed, Some("~/".into()));
    }

    #[test]
    fn nonexistent_parent_is_a_noop() {
        let got = complete_path("/definitely/not/a/real/dir/x", None);
        assert_eq!(got, PathCompletion::default());
    }

    fn names(dirs: &[DirEntry]) -> Vec<&str> {
        dirs.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn list_dirs_narrows_by_partial_and_hides_dotted() {
        let tmp = fixture();
        let got = list_dirs(&p(&tmp, "projects/"), None);
        assert_eq!(names(&got), vec!["alpha", "alps", "beta"]);
        let got = list_dirs(&p(&tmp, "projects/al"), None);
        assert_eq!(names(&got), vec!["alpha", "alps"]);
        // Dotted entries need a dotted partial, files never appear.
        let got = list_dirs(&p(&tmp, "projects/."), None);
        assert_eq!(names(&got), vec![".hidden"]);
        let got = list_dirs(&p(&tmp, "projects/no"), None);
        assert!(got.is_empty(), "notes.txt is a file");
    }

    #[test]
    fn list_dirs_matches_fuzzily_ranked_prefix_substring_subsequence() {
        let tmp = fixture();
        // "lp" is a substring of alpha/alps, "pa" a subsequence of alpha
        // only — and never a prefix, so the old prefix filter finds nothing.
        let got = list_dirs(&p(&tmp, "projects/lp"), None);
        assert_eq!(names(&got), vec!["alpha", "alps"]);
        let got = list_dirs(&p(&tmp, "projects/pa"), None);
        assert_eq!(names(&got), vec!["alpha"]);
        // Case-insensitive, and prefix matches outrank looser ones.
        let got = list_dirs(&p(&tmp, "projects/A"), None);
        assert_eq!(names(&got), vec!["alpha", "alps", "beta"]);
        assert!(got[0].name.starts_with('a'), "prefix matches sort first");
    }

    #[test]
    fn deep_scan_finds_nested_dirs_and_stops_inside_repos() {
        let tmp = fixture();
        // projects/alpha is a repo whose insides must not be scanned;
        // projects/beta/nested is fair game two levels down.
        std::fs::create_dir_all(tmp.path().join("projects/alpha/.git")).unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/alpha/src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/beta/nested")).unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/node_modules/junk")).unwrap();
        let scanned = scan_parent(&p(&tmp, "x"), None);
        let names: Vec<&str> = scanned.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"projects/beta/nested"), "{names:?}");
        assert!(names.contains(&"projects/alpha"), "{names:?}");
        assert!(
            !names.contains(&"projects/alpha/src"),
            "repo is a leaf: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("node_modules")),
            "{names:?}"
        );

        // A typed partial matches nested basenames; repos outrank plain
        // dirs at equal match quality.
        let got = filter_deep(&scanned, "nes");
        assert_eq!(got[0].name, "projects/beta/nested");
        let got = filter_deep(&scanned, "alp");
        assert_eq!(got[0].name, "projects/alpha", "repo first: {got:?}");
        assert!(got[0].is_repo);

        // Multi-word: every token must land — basename or path, any order.
        let got = filter_deep(&scanned, "nested projects");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].name, "projects/beta/nested");
        assert!(filter_deep(&scanned, "nested nowhere").is_empty());
    }

    #[test]
    fn match_positions_cover_substring_and_subsequence() {
        assert_eq!(match_positions("nebula", "neb"), vec![0, 1, 2]);
        assert_eq!(match_positions("my-work", "wrk"), vec![3, 5, 6]);
        assert_eq!(
            match_positions("Nebula", "neb"),
            vec![0, 1, 2],
            "case-insensitive"
        );
        assert!(match_positions("nebula", "").is_empty());
        assert!(match_positions("nebula", "xyz").is_empty());
        // Multi-token queries highlight the union of their matches.
        assert_eq!(
            match_positions("Desktop/nebula", "neb desk"),
            vec![0, 1, 2, 3, 8, 9, 10]
        );
    }

    #[test]
    fn list_dirs_marks_git_repos() {
        let tmp = fixture();
        std::fs::create_dir_all(tmp.path().join("projects/alpha/.git")).unwrap();
        // Linked worktrees keep `.git` as a file — still a repo.
        std::fs::write(tmp.path().join("projects/beta/.git"), "gitdir: x").unwrap();
        let got = list_dirs(&p(&tmp, "projects/"), None);
        let repos: Vec<(&str, bool)> = got.iter().map(|d| (d.name.as_str(), d.is_repo)).collect();
        assert_eq!(
            repos,
            vec![("alpha", true), ("alps", false), ("beta", true)]
        );
    }

    #[test]
    fn list_dirs_expands_tilde_and_survives_bad_parents() {
        let tmp = fixture();
        let got = list_dirs("~/projects/al", Some(tmp.path()));
        assert_eq!(names(&got), vec!["alpha", "alps"]);
        assert!(list_dirs("/definitely/not/a/real/dir/", None).is_empty());
    }
}

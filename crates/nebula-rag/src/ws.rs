//! Workspace paths: `shared`, `apps/<name>/docs`, … — lowercase segments of
//! `[a-z0-9._-]`, no leading or trailing slash. `/` is the root: the whole
//! index, never a stored row.

use anyhow::{bail, Result};

/// `None` for the root.
pub fn normalize(path: &str) -> Result<Option<String>> {
    let trimmed = path.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(None);
    }
    let lower = trimmed.to_lowercase();
    for seg in lower.split('/') {
        let ok = !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !ok {
            bail!("bad workspace path {path:?}: segments are [a-z0-9._-], separated by /");
        }
    }
    Ok(Some(lower))
}

pub fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(p, _)| p)
}

pub fn leaf(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, l)| l).unwrap_or(path)
}

/// `path` equals `root` or lies beneath it.
pub fn in_subtree(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `a` and `b` name the same thing closely enough that creating one
/// next to the other is probably a duplicate: same after dropping
/// separators and a plural `s`, a small edit distance, or one a prefix of
/// the other.
pub fn similar(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        s.strip_suffix('s').map(str::to_string).unwrap_or(s)
    };
    let (a, b) = (norm(a), norm(b));
    if a == b {
        return true;
    }
    let short = a.len().min(b.len());
    if short >= 4 && (a.starts_with(&b) || b.starts_with(&a)) {
        return true;
    }
    let max = if short <= 5 { 1 } else { 2 };
    levenshtein(&a, &b) <= max
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_rejects_bad_paths() {
        assert_eq!(
            normalize("/Apps/Nebula/").unwrap().as_deref(),
            Some("apps/nebula")
        );
        assert_eq!(normalize("/").unwrap(), None);
        assert!(normalize("apps/../x").is_err());
        assert!(normalize("apps/a b").is_err());
        assert!(normalize("apps//x").is_err());
    }

    #[test]
    fn subtree_is_slash_delimited() {
        assert!(in_subtree("apps/nebula/docs", "apps/nebula"));
        assert!(in_subtree("apps/nebula", "apps/nebula"));
        assert!(!in_subtree("apps/nebula2", "apps/nebula"));
    }

    #[test]
    fn near_duplicate_names() {
        assert!(similar("note", "notes"));
        assert!(similar("conventions", "convention"));
        assert!(similar("infra", "infa"));
        assert!(similar("rust-tips", "rusttips"));
        assert!(similar("deploy", "deployment"));
        assert!(!similar("docs", "notes"));
        assert!(!similar("rust", "infra"));
        assert!(!similar("vela", "nebula"));
    }
}

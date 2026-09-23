//! The subset of gitignore syntax the docs sync needs, applied to paths
//! read from a commit (so `git check-ignore`, which reads the working tree,
//! can't be used): `#` comments, `!` negation, trailing `/` for
//! directories, a leading or inner `/` anchors to the repo root, `*`, `?`,
//! `**` and `[...]` classes. Last match wins; an ignored directory hides
//! everything under it.

#[derive(Debug)]
struct Rule {
    glob: Vec<char>,
    negate: bool,
    dir_only: bool,
    anchored: bool,
}

#[derive(Debug, Default)]
pub struct Ignore {
    rules: Vec<Rule>,
}

impl Ignore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the lines of one ignore file; later files override earlier ones.
    pub fn add(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negate, pat) = match line.strip_prefix('!') {
                Some(p) => (true, p),
                None => (false, line.strip_prefix('\\').unwrap_or(line)),
            };
            let (dir_only, pat) = match pat.strip_suffix('/') {
                Some(p) => (true, p),
                None => (false, pat),
            };
            let anchored = pat.contains('/');
            let pat = pat.strip_prefix('/').unwrap_or(pat);
            if pat.is_empty() {
                continue;
            }
            self.rules.push(Rule {
                glob: pat.chars().collect(),
                negate,
                dir_only,
                anchored,
            });
        }
    }

    pub fn is_ignored(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        for i in 1..parts.len() {
            if self.verdict(&parts[..i].join("/"), true) == Some(true) {
                return true;
            }
        }
        self.verdict(path, false) == Some(true)
    }

    fn verdict(&self, path: &str, is_dir: bool) -> Option<bool> {
        let base = path.rsplit('/').next().unwrap_or(path);
        let mut out = None;
        for r in &self.rules {
            if r.dir_only && !is_dir {
                continue;
            }
            let target = if r.anchored { path } else { base };
            if glob(&r.glob, &target.chars().collect::<Vec<_>>()) {
                out = Some(!r.negate);
            }
        }
        out
    }
}

fn glob(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') if p.get(1) == Some(&'*') => {
            // `**/` matches zero or more directories; `**` anything.
            let rest = if p.get(2) == Some(&'/') {
                &p[3..]
            } else {
                &p[2..]
            };
            (0..=s.len())
                .any(|i| (i == 0 || s[i - 1] == '/' || rest.is_empty()) && glob(rest, &s[i..]))
        }
        Some('*') => (0..=s.len())
            .take_while(|&i| i == 0 || s[i - 1] != '/')
            .any(|i| glob(&p[1..], &s[i..])),
        Some('?') => s.first().is_some_and(|c| *c != '/') && glob(&p[1..], &s[1..]),
        Some('[') => {
            let Some(end) = p.iter().skip(1).position(|c| *c == ']').map(|e| e + 1) else {
                return s.first() == Some(&'[') && glob(&p[1..], &s[1..]);
            };
            let Some(c) = s.first() else { return false };
            let class = &p[1..end];
            let (neg, class) = match class.first() {
                Some('!' | '^') => (true, &class[1..]),
                _ => (false, class),
            };
            let mut hit = false;
            let mut i = 0;
            while i < class.len() {
                if i + 2 < class.len() && class[i + 1] == '-' {
                    hit |= (class[i]..=class[i + 2]).contains(c);
                    i += 3;
                } else {
                    hit |= class[i] == *c;
                    i += 1;
                }
            }
            hit != neg && glob(&p[end + 1..], &s[1..])
        }
        Some(ch) => s.first() == Some(ch) && glob(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ig(text: &str) -> Ignore {
        let mut i = Ignore::new();
        i.add(text);
        i
    }

    #[test]
    fn basename_patterns_match_at_any_depth() {
        let i = ig("*.draft.md\nnode_modules/\n");
        assert!(i.is_ignored("docs/x.draft.md"));
        assert!(i.is_ignored("web/node_modules/pkg/README.md"));
        assert!(!i.is_ignored("docs/x.md"));
    }

    #[test]
    fn slashes_anchor_to_the_root() {
        let i = ig("/vendor\ndocs/internal/\n");
        assert!(i.is_ignored("vendor/vt100/README.md"));
        assert!(!i.is_ignored("crates/vendor/README.md"));
        assert!(i.is_ignored("docs/internal/a.md"));
        assert!(!i.is_ignored("x/docs/internal/a.md"));
    }

    #[test]
    fn negation_and_double_star_and_classes() {
        let i = ig("docs/**/*.md\n!docs/**/keep.md\n[Cc]HANGELOG.md\n");
        assert!(i.is_ignored("docs/a/b/c.md"));
        assert!(i.is_ignored("docs/c.md"));
        assert!(!i.is_ignored("docs/a/keep.md"));
        assert!(i.is_ignored("CHANGELOG.md"));
        assert!(!i.is_ignored("changelog.md"));
        assert!(i.is_ignored("sub/cHANGELOG.md"));
    }

    #[test]
    fn a_file_under_an_ignored_directory_cannot_be_re_included() {
        let i = ig("build/\n!build/keep.md\n");
        assert!(i.is_ignored("build/keep.md"));
    }
}

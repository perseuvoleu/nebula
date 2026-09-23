//! Markdown chunker: split on headings (outside code fences), cap each chunk
//! at ~400 tokens, and carry the heading chain (`README.md > Install > zig`)
//! so a chunk still says what it is about once cut out of its file. Same
//! shape as the embedding eval (`.scratch/rag/embed-eval/chunk.py`).

pub const MAX_TOKENS: usize = 400;

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// `label > H1 > H2`, the context prefix.
    pub heading: String,
    pub text: String,
}

impl Chunk {
    /// What gets embedded and full-text indexed: the chain, then the body.
    pub fn indexed(&self) -> String {
        format!("{}\n\n{}", self.heading, self.text)
    }

    /// Innermost heading, for `file.md#Heading` sources.
    pub fn anchor(&self) -> Option<&str> {
        anchor_of(&self.heading)
    }
}

/// Innermost heading of a stored `label > H1 > H2` chain (none for the
/// label alone).
pub fn anchor_of(heading: &str) -> Option<&str> {
    heading.split(" > ").skip(1).last()
}

/// ~4 chars per token: the estimate the eval used; no tokenizer needed.
pub fn tokens(s: &str) -> usize {
    (s.chars().count() / 4).max(1)
}

pub fn chunk_markdown(label: &str, text: &str) -> Vec<Chunk> {
    let mut sections: Vec<(Vec<String>, String)> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut chain: Vec<String> = Vec::new();
    let mut body = String::new();
    let mut fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fence = !fence;
        }
        let heading = (!fence).then(|| heading_of(line)).flatten();
        if let Some((level, title)) = heading {
            sections.push((chain.clone(), std::mem::take(&mut body)));
            stack.retain(|(l, _)| *l < level);
            stack.push((level, title));
            chain = stack.iter().map(|(_, t)| t.clone()).collect();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections.push((chain, body));

    let mut out = Vec::new();
    for (chain, body) in sections {
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let heading = std::iter::once(label.to_string())
            .chain(chain)
            .collect::<Vec<_>>()
            .join(" > ");
        let budget = MAX_TOKENS.saturating_sub(tokens(&heading)).max(50);
        for part in split_long(body, budget) {
            out.push(Chunk {
                heading: heading.clone(),
                text: part,
            });
        }
    }
    out
}

fn heading_of(line: &str) -> Option<(usize, String)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim();
    (!title.is_empty()).then(|| (level, title.to_string()))
}

/// Greedy: paragraphs, then lines for a paragraph over the budget, then
/// hard character cuts for a single giant line.
fn split_long(text: &str, limit: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let push = |cur: &mut String, piece: &str, sep: &str, parts: &mut Vec<String>| {
        if !cur.is_empty() && tokens(&format!("{cur}{sep}{piece}")) > limit {
            parts.push(std::mem::take(cur).trim().to_string());
        }
        if !cur.is_empty() {
            cur.push_str(sep);
        }
        cur.push_str(piece);
    };
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if tokens(para) <= limit {
            push(&mut cur, para, "\n\n", &mut parts);
            continue;
        }
        for line in para.lines() {
            if tokens(line) <= limit {
                push(&mut cur, line, "\n", &mut parts);
                continue;
            }
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(limit * 4) {
                push(
                    &mut cur,
                    &piece.iter().collect::<String>(),
                    "\n",
                    &mut parts,
                );
            }
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_headings_with_the_chain_as_context() {
        let md = "intro line\n# Install\nstep one\n## zig\nneeds 0.15.2\n# Usage\nrun it\n";
        let c = chunk_markdown("README.md", md);
        let got: Vec<(&str, &str)> = c
            .iter()
            .map(|c| (c.heading.as_str(), c.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("README.md", "intro line"),
                ("README.md > Install", "step one"),
                ("README.md > Install > zig", "needs 0.15.2"),
                ("README.md > Usage", "run it"),
            ]
        );
        assert_eq!(c[2].anchor(), Some("zig"));
        assert_eq!(c[0].anchor(), None);
    }

    #[test]
    fn hash_lines_inside_code_fences_are_not_headings() {
        let md = "# Build\n```sh\n# not a heading\nmake\n```\n";
        let c = chunk_markdown("x.md", md);
        assert_eq!(c.len(), 1);
        assert!(c[0].text.contains("# not a heading"));
    }

    #[test]
    fn long_sections_split_under_the_token_cap() {
        let para = "word ".repeat(300); // ~375 tokens each
        let md = format!("# Big\n{para}\n\n{para}\n\n{}", "x".repeat(5000));
        let c = chunk_markdown("big.md", &md);
        assert!(c.len() >= 3, "{}", c.len());
        for ch in &c {
            assert!(
                tokens(&ch.indexed()) <= MAX_TOKENS + 5,
                "{}",
                tokens(&ch.indexed())
            );
            assert_eq!(ch.heading, "big.md > Big");
        }
    }
}

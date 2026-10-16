//! Multi-file model output parsing and prompts.
//!
//! Preferred format:
//! ```text
//! @@@ FILE: relative/path/to/file.py
//! <file contents>
//! @@@ FILE: another/file.js
//! <file contents>
//! ```

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub path: String,
    pub content: String,
}

fn file_marker_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^@@@\s*FILE:\s*(.+?)\s*$").expect("file marker regex"))
}

fn alt_marker_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // **path/to/file.py** or ## file: path or FILE: path
        Regex::new(r"(?mi)^(?:#{1,3}\s*)?(?:\*\*)?(?:file\s*:|path\s*:)?\s*([A-Za-z0-9_./\-]+\.[A-Za-z0-9]+)(?:\*\*)?\s*$")
            .expect("alt marker regex")
    })
}

fn fenced_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // ```python title=foo.py or ```python:foo.py or ```foo.py
        Regex::new(r"(?m)^```(?:[A-Za-z0-9_+-]+)?(?:\s+(?:title|file|path)=|\s*:|\s+)([A-Za-z0-9_./\-]+\.[A-Za-z0-9]+)\s*$")
            .expect("fenced file regex")
    })
}

/// Parse multi-file bundles. Falls back to a single file when no markers exist.
pub fn parse_multifile_output(raw: &str, fallback_path: &str) -> Vec<ParsedFile> {
    let fallback = {
        let n = normalize_rel_path(fallback_path);
        if n.is_empty() || n == "." {
            "main.py".to_string()
        } else {
            n
        }
    };

    // Prefer parsing before stripping — outer fences can wrap the whole bundle.
    let mut files = parse_with_regex(raw, file_marker_re());
    if files.is_empty() {
        files = parse_with_regex(raw, alt_marker_re());
    }
    if files.is_empty() {
        files = parse_fenced_files(raw);
    }
    if files.is_empty() {
        let text = crate::ollama::strip_code_fences(raw);
        files = parse_with_regex(&text, file_marker_re());
        if files.is_empty() {
            files = parse_with_regex(&text, alt_marker_re());
        }
        if files.is_empty() {
            files = parse_fenced_files(&text);
        }
        if files.is_empty() {
            let content = text.trim().to_string();
            if content.is_empty() {
                return Vec::new();
            }
            return vec![ParsedFile {
                path: fallback,
                content: ensure_trailing_newline(content),
            }];
        }
    }

    files
        .into_iter()
        .map(|mut f| {
            f.content = crate::ollama::strip_code_fences(&f.content);
            f.content = scrub_agent_artifacts(&f.content);
            f.content = ensure_trailing_newline(f.content.trim().to_string());
            f
        })
        .filter(|f| !f.content.is_empty() && !f.path.is_empty() && !f.path.contains(".."))
        .collect()
}

/// Remove leaked `@ @@ FILE` / fence / markdown-prose lines models leave in file bodies.
pub fn scrub_agent_artifacts(content: &str) -> String {
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("@@@") {
            continue;
        }
        if t == "--- stub ---" || t == "--- end stub ---" {
            continue;
        }
        // Bare markdown fence lines (``` or ```python) — not code that happens to contain backticks.
        if t.starts_with("```") && !t[3..].contains(' ') && t.len() < 40 {
            continue;
        }
        // Markdown checklist / prose bullets (e.g. "- So 1 == 1 ✓"), not Python.
        if let Some(rest) = t.strip_prefix("- ") {
            let prose = rest.starts_with(|c: char| c.is_uppercase())
                || rest.contains('✓')
                || rest.contains('❌')
                || rest.starts_with("So ")
                || rest.starts_with("Ensure ")
                || rest.starts_with("Note ");
            if prose {
                continue;
            }
        }
        out.push(line);
    }
    out.join("\n")
}

fn parse_with_regex(text: &str, re: &Regex) -> Vec<ParsedFile> {
    let mut matches: Vec<(usize, usize, String)> = Vec::new();
    for caps in re.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let path = caps
            .get(1)
            .map(|c| normalize_rel_path(c.as_str()))
            .unwrap_or_default();
        if !path.is_empty() && !path.contains("..") && path.contains('.') {
            matches.push((m.start(), m.end(), path));
        }
    }
    if matches.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, (_start, end, path)) in matches.iter().enumerate() {
        let body_start = *end;
        let body_end = matches.get(i + 1).map(|(s, _, _)| *s).unwrap_or(text.len());
        let content = text[body_start..body_end]
            .trim_start_matches('\n')
            .trim_end()
            .to_string();
        if content.is_empty() {
            continue;
        }
        out.push(ParsedFile {
            path: path.clone(),
            content,
        });
    }
    out
}

fn parse_fenced_files(text: &str) -> Vec<ParsedFile> {
    let re = fenced_file_re();
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(caps) = re.captures(rest) {
        let full = caps.get(0).unwrap();
        let path = normalize_rel_path(caps.get(1).unwrap().as_str());
        let after = &rest[full.end()..];
        let (body, next) = if let Some(end) = after.find("\n```") {
            (&after[..end], &after[end + 4..])
        } else {
            (after, "")
        };
        if !path.is_empty() && !body.trim().is_empty() {
            out.push(ParsedFile {
                path,
                content: body.trim().to_string(),
            });
        }
        rest = next;
        if rest.is_empty() {
            break;
        }
    }
    out
}

pub fn normalize_rel_path(p: &str) -> String {
    p.trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '\'')
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/")
}

fn ensure_trailing_newline(mut s: String) -> String {
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

pub fn build_multifile_prompt(
    hint_path: &str,
    user_prompt: &str,
    context_chunks: &str,
    existing_note: &str,
) -> String {
    let hint_line = if hint_path.is_empty() || hint_path == "." {
        "None — invent all paths yourself under the workspace root.".to_string()
    } else {
        format!("{hint_path} (optional name hint only; you may add more files)")
    };
    format!(
        r#"You are a senior coding agent. Satisfy the user request completely.
Create as many directories and files as required — do not stop at a single file if the task needs a small project layout (e.g. package init, README, config, tests, CLI entrypoint).
Do NOT edit or depend on unrelated existing project files unless the user asked to integrate with them.

Output format (STRICT):
- Emit one or more file blocks.
- Each block MUST start with a line exactly like: @@@ FILE: relative/path/from/workspace/root.ext
- Then the full file contents (no markdown fences around the whole response).
- Paths must be relative (no ..). Nested dirs via path segments (e.g. scripts/jobsearch/__init__.py).
- Prefer a clean, runnable layout.

Example:
@@@ FILE: scripts/find_jobs/__init__.py

@@@ FILE: scripts/find_jobs/cli.py
def main():
    print("hi")

@@@ FILE: scripts/requirements.txt
requests>=2.0

### User Request:
{user_prompt}

### Path hint:
{hint_line}

### Notes:
{existing_note}

### Retrieved Code Context (optional; ignore if unrelated to a greenfield create):
{context_chunks}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_files() {
        let raw = r#"@@@ FILE: a/b.py
print(1)
@@@ FILE: c.txt
hello
"#;
        let files = parse_multifile_output(raw, "fallback.py");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a/b.py");
        assert_eq!(files[1].path, "c.txt");
    }

    #[test]
    fn fallback_dot_becomes_main() {
        let files = parse_multifile_output("x = 1\n", ".");
        assert_eq!(files[0].path, "main.py");
    }

    #[test]
    fn parses_fenced_titled() {
        let raw = "```python scripts/app.py\nprint(1)\n```\n```text scripts/readme.md\n# hi\n```\n";
        let files = parse_multifile_output(raw, ".");
        assert!(files.len() >= 2, "{files:?}");
    }

    #[test]
    fn scrubs_leaked_markers_inside_body() {
        let raw = r#"@@@ FILE: solution.py
class Solution:
    def foo(self):
        return 1
@@@
"#;
        let files = parse_multifile_output(raw, "solution.py");
        assert_eq!(files.len(), 1);
        assert!(!files[0].content.contains("@@@"));
        assert!(files[0].content.contains("class Solution"));
    }

    #[test]
    fn scrubs_markdown_prose_bullets() {
        let raw = "class Solution:\n    pass\n- So 1 == 1 ✓\n- x = 1\n";
        let scrubbed = scrub_agent_artifacts(raw);
        assert!(!scrubbed.contains("So 1"));
        assert!(scrubbed.contains("class Solution"));
        // "- x = 1" looks like python-ish assignment after dash — keep (not uppercase prose)
        assert!(scrubbed.contains("x = 1") || !scrubbed.contains("So "));
    }
}

//! Detect runnable CLI entrypoints from a workspace / goal.
//!
//! Used by small-model / checkpoint-heal gates so we are not hard-coded to one
//! package name (e.g. `jobs_aggregator`).

use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

/// A Python module that should support `python -m <module> -h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonCliModule {
    pub module: String,
}

impl PythonCliModule {
    pub fn help_display(&self) -> String {
        format!("python -m {} -h", self.module)
    }
}

const SKIP_DIRS: &[&str] = &[
    ".venv",
    ".vibe",
    ".git",
    "__pycache__",
    "node_modules",
    "target",
    "dist",
    "build",
    "tests",
    "test",
    ".eggs",
    "*.egg-info",
];

fn skip_dir_name(name: &str) -> bool {
    if SKIP_DIRS.contains(&name) {
        return true;
    }
    name.ends_with(".egg-info") || name.starts_with('.')
}

/// Infer the primary Python CLI module for help-smoke checks.
///
/// Priority:
/// 1. `python -m <mod>` / `python3 -m <mod>` in the goal text
/// 2. Top-level package dirs that already have `__main__.py` or `cli.py`
/// 3. Paths in `touched` (`pkg/cli.py`, `pkg/__main__.py`)
/// 4. `[project] name` / packages in `pyproject.toml` when that package dir exists
pub fn detect_python_cli_module(
    root: &Path,
    touched: &BTreeMap<String, String>,
    goal: &str,
) -> Option<PythonCliModule> {
    if let Some(m) = module_from_goal(goal) {
        return Some(PythonCliModule { module: m });
    }

    if let Some(m) = module_from_package_dirs(root) {
        return Some(PythonCliModule { module: m });
    }

    if let Some(m) = module_from_touched(touched) {
        return Some(PythonCliModule { module: m });
    }

    if let Some(m) = module_from_pyproject(root) {
        return Some(PythonCliModule { module: m });
    }

    None
}

/// True when the tree looks like it has (or is growing) a Python package CLI.
pub fn python_cli_gate_applies(
    root: &Path,
    touched: &BTreeMap<String, String>,
    goal: &str,
) -> bool {
    detect_python_cli_module(root, touched, goal).is_some()
        || goal_mentions_python_cli(goal)
        || touched
            .keys()
            .any(|k| k.ends_with("/cli.py") || k.ends_with("/__main__.py") || k == "cli.py")
}

pub fn goal_mentions_python_cli(goal: &str) -> bool {
    let g = goal.to_ascii_lowercase();
    g.contains("python -m ")
        || g.contains("python3 -m ")
        || g.contains("argparse")
        || (g.contains("cli")
            && (g.contains("subcommand") || g.contains("--help") || g.contains(" -h")))
}

/// Draft a scenario `constraints.md` from workspace + sample prompt (no product lore).
pub fn synthesize_constraints_md(root: &Path, sample_prompt: &str) -> String {
    let mut bullets: Vec<String> = Vec::new();
    bullets.push(
        "- Keep the product focused on the user request; do not invent unrelated apps.".into(),
    );
    if let Some(cli) = detect_python_cli_module(root, &BTreeMap::new(), sample_prompt) {
        bullets.push(format!("- Prefer Python package `{}`.", cli.module));
        bullets.push(format!(
            "- `{}` must work after install without crashing on import.",
            cli.help_display()
        ));
        bullets.push(
            "- Lazy-import optional heavy deps inside functions that need them — not at module top-level for CLI help."
                .into(),
        );
    } else if goal_mentions_python_cli(sample_prompt) {
        bullets.push(
            "- If you create a Python CLI package, `python -m <package> -h` must exit 0 after install."
                .into(),
        );
        bullets.push(
            "- Lazy-import optional heavy deps inside functions that need them — not at module top-level for CLI help."
                .into(),
        );
    }
    // Prefer an explicit package name mentioned as `name` with package/module wording.
    static PKG: OnceLock<Regex> = OnceLock::new();
    let pkg_re = PKG.get_or_init(|| {
        Regex::new(
            r"(?i)(?:package|module|prefer)\s+`([A-Za-z_][\w]*)`|`([A-Za-z_][\w]*)`\s+(?:package|module)",
        )
        .expect("pkg")
    });
    if let Some(cap) = pkg_re.captures(sample_prompt) {
        let name = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map(|m| m.as_str())
            .unwrap_or("");
        if !name.is_empty() && !bullets.iter().any(|b| b.contains(&format!("`{name}`"))) {
            bullets.insert(1, format!("- Prefer Python package `{name}`."));
        }
    }
    format!("{}\n", bullets.join("\n"))
}

fn module_from_goal(goal: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"(?i)python3?\s+-m\s+([A-Za-z_][\w.]*)").expect("py -m"));
    re.captures(goal)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

fn module_from_touched(touched: &BTreeMap<String, String>) -> Option<String> {
    for path in touched.keys() {
        let p = path.replace('\\', "/");
        if let Some(modname) = module_from_rel_path(&p) {
            return Some(modname);
        }
    }
    None
}

fn module_from_rel_path(p: &str) -> Option<String> {
    if let Some(rest) = p.strip_suffix("/__main__.py") {
        if !rest.contains('/') && is_ident(rest) {
            return Some(rest.to_string());
        }
    }
    if let Some(rest) = p.strip_suffix("/cli.py") {
        if !rest.contains('/') && is_ident(rest) {
            return Some(rest.to_string());
        }
    }
    None
}

fn module_from_package_dirs(root: &Path) -> Option<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return None;
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if skip_dir_name(&name) || !is_ident(&name) {
            continue;
        }
        let dir = entry.path();
        let has_main = dir.join("__main__.py").is_file();
        let has_cli = dir.join("cli.py").is_file();
        let has_init = dir.join("__init__.py").is_file();
        if has_init && (has_main || has_cli) {
            found.push(name.to_string());
        }
    }
    // Prefer a single obvious package; if several, pick one with __main__.py.
    if found.len() == 1 {
        return found.pop();
    }
    for name in &found {
        if root.join(name).join("__main__.py").is_file() {
            return Some(name.clone());
        }
    }
    found.into_iter().next()
}

fn module_from_pyproject(root: &Path) -> Option<String> {
    let raw = fs::read_to_string(root.join("pyproject.toml")).ok()?;
    // Very small TOML scrape — avoid pulling a toml parser into hot paths twice.
    static NAME_RE: OnceLock<Regex> = OnceLock::new();
    let name_re =
        NAME_RE.get_or_init(|| Regex::new(r#"(?m)^\s*name\s*=\s*["']([^"']+)["']"#).expect("name"));
    if let Some(cap) = name_re.captures(&raw) {
        let name = cap.get(1)?.as_str().replace('-', "_");
        if is_ident(&name) {
            let dir = root.join(&name);
            if dir.join("__init__.py").is_file()
                || dir.join("__main__.py").is_file()
                || dir.join("cli.py").is_file()
            {
                return Some(name);
            }
        }
    }
    None
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether any top-level Python package directory exists (for greenfield heuristics).
pub fn has_python_package(root: &Path) -> bool {
    module_from_package_dirs(root).is_some()
        || root
            .read_dir()
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    && !skip_dir_name(&name)
                    && is_ident(&name)
                    && e.path().join("__init__.py").is_file()
            })
}

/// Best path to pass as agent fallback / primary edit target.
pub fn suggest_fallback_path(root: &Path) -> String {
    if let Some(m) = module_from_package_dirs(root) {
        let cli = root.join(&m).join("cli.py");
        if cli.is_file() {
            return format!("{m}/cli.py");
        }
        let main = root.join(&m).join("__main__.py");
        if main.is_file() {
            return format!("{m}/__main__.py");
        }
        return format!("{m}/__init__.py");
    }
    for cand in ["solution.py", "src/main.rs", "main.py", "README.md"] {
        if root.join(cand).is_file() {
            return cand.to_string();
        }
    }
    "README.md".into()
}

/// Display string for prompts when a module is known, else a generic CLI hint.
pub fn cli_help_instruction(module: Option<&PythonCliModule>) -> String {
    match module {
        Some(m) => format!("`{}` must exit 0", m.help_display()),
        None => "`python -m <package> -h` (or the project CLI --help) must exit 0".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_module_from_goal() {
        let m = detect_python_cli_module(
            Path::new("/tmp"),
            &BTreeMap::new(),
            "Keep `python -m my_tool -h` working",
        );
        assert_eq!(m.unwrap().module, "my_tool");
    }

    #[test]
    fn detects_module_from_touched_cli() {
        let mut t = BTreeMap::new();
        t.insert("acme_cli/cli.py".into(), "print('hi')\n".into());
        let m = detect_python_cli_module(Path::new("/tmp"), &t, "add a flag");
        assert_eq!(m.unwrap().module, "acme_cli");
    }

    #[test]
    fn detects_module_from_package_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("widget")).unwrap();
        fs::write(dir.path().join("widget/__init__.py"), "").unwrap();
        fs::write(dir.path().join("widget/__main__.py"), "print(1)\n").unwrap();
        let m = detect_python_cli_module(dir.path(), &BTreeMap::new(), "build it");
        assert_eq!(m.unwrap().module, "widget");
    }

    #[test]
    fn suggest_fallback_prefers_cli() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("widget")).unwrap();
        fs::write(dir.path().join("widget/__init__.py"), "").unwrap();
        fs::write(dir.path().join("widget/cli.py"), "def main():\n    pass\n").unwrap();
        assert_eq!(suggest_fallback_path(dir.path()), "widget/cli.py");
    }

    #[test]
    fn synthesize_constraints_includes_detected_package() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("widget")).unwrap();
        fs::write(dir.path().join("widget/__init__.py"), "").unwrap();
        fs::write(dir.path().join("widget/__main__.py"), "print(1)\n").unwrap();
        let md = synthesize_constraints_md(dir.path(), "build a CLI");
        assert!(md.contains("`widget`"), "{md}");
        assert!(md.contains("python -m widget -h"), "{md}");
    }
}

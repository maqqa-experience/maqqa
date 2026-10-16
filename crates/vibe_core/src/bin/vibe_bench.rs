//! Coding-ability bench for the vibe agent (hand fixtures + LeetCodeDataset).
//!
//! Requires Ollama + configured reason model (except `--check-stubs` / `--list`).
//!
//! Usage:
//!   cargo run -p vibe_core --bin vibe_bench -- --list
//!   cargo run -p vibe_core --bin vibe_bench -- --check-stubs
//!   cargo run -p vibe_core --bin vibe_bench -- --source lcd --split test --limit 20
//!   cargo run -p vibe_core --bin vibe_bench -- --source all --difficulty Easy --limit 10
//!   cargo run -p vibe_core --bin vibe_bench -- --only two_sum,valid_parentheses --json results.json
//!
//! Import corpus first:
//!   python3 scripts/import_leetcode.py

use flate2::read::GzDecoder;
use regex::Regex;
use serde::Deserialize;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};
use vibe_core::agent::AgentLoop;
use vibe_core::config::VibeConfig;
use vibe_core::ollama::OllamaClient;
use vibe_core::shadow::ShadowWorkspace;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Python,
    Rust,
}

#[derive(Clone)]
struct Challenge {
    id: String,
    kind: Kind,
    /// Hand fixture directory name under fixtures/coding/, if any.
    hand_dir: Option<String>,
    /// Slim LeetCodeDataset row, if any.
    lcd: Option<LcdProblem>,
}

#[derive(Clone, Debug, Deserialize)]
struct LcdProblem {
    task_id: String,
    #[serde(default)]
    difficulty: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    problem_description: String,
    starter_code: String,
    prompt: String,
    entry_point: String,
    test: String,
    #[serde(default)]
    query: String,
}

#[derive(Debug)]
struct CaseResult {
    id: String,
    passed: bool,
    seconds: f64,
    summary: String,
    files: Vec<String>,
    test_output: String,
    difficulty: String,
    source: String,
}

struct Cli {
    list: bool,
    check_stubs: bool,
    only: Option<Vec<String>>,
    source: String,
    split: String,
    difficulty: Option<Vec<String>>,
    limit: Option<usize>,
    json_out: Option<String>,
    lcd_version: String,
}

fn parse_cli(args: &[String]) -> Cli {
    let flag = |name: &str| args.iter().any(|a| a == name);
    let val = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    Cli {
        list: flag("--list"),
        check_stubs: flag("--check-stubs"),
        only: val("--only").map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        }),
        source: val("--source").unwrap_or_else(|| "hand".into()),
        split: val("--split").unwrap_or_else(|| "test".into()),
        difficulty: val("--difficulty").map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        }),
        limit: val("--limit").and_then(|s| s.parse().ok()),
        json_out: val("--json"),
        lcd_version: val("--lcd-version").unwrap_or_else(|| "v0.3.1".into()),
    }
}

fn hand_challenges() -> Vec<Challenge> {
    const HAND: &[(&str, &str, Kind)] = &[
        ("two_sum", "two_sum", Kind::Python),
        ("valid_parentheses", "valid_parentheses", Kind::Python),
        ("max_profit", "max_profit", Kind::Python),
        ("merge_intervals", "merge_intervals", Kind::Python),
        ("rust_two_sum", "rust_two_sum", Kind::Rust),
    ];
    HAND.iter()
        .map(|(id, dir, kind)| Challenge {
            id: (*id).into(),
            kind: *kind,
            hand_dir: Some((*dir).into()),
            lcd: None,
        })
        .collect()
}

fn load_lcd_jsonl(path: &Path) -> Result<Vec<LcdProblem>, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let reader: Box<dyn BufRead> = if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("gz"))
        .unwrap_or(false)
    {
        Box::new(BufReader::new(GzDecoder::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("readline {i}: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let row: LcdProblem =
            serde_json::from_str(&line).map_err(|e| format!("json line {}: {e}", i + 1))?;
        out.push(row);
    }
    Ok(out)
}

fn lcd_challenges(
    fixtures_leetcode: &Path,
    version: &str,
    split: &str,
) -> Result<Vec<Challenge>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for part in split.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "all" {
            paths.push(fixtures_leetcode.join(format!("{version}-test.jsonl.gz")));
            paths.push(fixtures_leetcode.join(format!("{version}-train.jsonl.gz")));
            continue;
        }
        let gz = fixtures_leetcode.join(format!("{version}-{part}.jsonl.gz"));
        let plain = fixtures_leetcode.join(format!("{version}-{part}.jsonl"));
        if gz.exists() {
            paths.push(gz);
        } else if plain.exists() {
            paths.push(plain);
        } else {
            return Err(format!(
                "missing LCD corpus {} (run: python3 scripts/import_leetcode.py)",
                gz.display()
            ));
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        for row in load_lcd_jsonl(&path)? {
            if !seen.insert(row.task_id.clone()) {
                continue;
            }
            out.push(Challenge {
                id: row.task_id.clone(),
                kind: Kind::Python,
                hand_dir: None,
                lcd: Some(row),
            });
        }
    }
    Ok(out)
}

fn materialize_lcd(problem: &LcdProblem, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let stub = format!(
        "{}\n{}\n        raise NotImplementedError(\"implement\")\n",
        problem.prompt.trim_end(),
        problem.starter_code.trim_end()
    );
    fs::write(dest.join("solution.py"), stub).map_err(|e| e.to_string())?;

    // Upstream HumanEval-style checks often reference bare `inf`/`nan` (injected in
    // their exec sandbox). Define them so unittest imports don't NameError.
    // Rewrite `assert candidate(...) == x` so failures print got vs expected.
    let test = format!(
        r#"import unittest

from solution import Solution

inf = float("inf")
nan = float("nan")

{test}

class TestLCD(unittest.TestCase):
    def test_all(self):
        check({entry})


if __name__ == "__main__":
    unittest.main()
"#,
        test = enrich_lcd_check_asserts(problem.test.trim()),
        entry = problem.entry_point.trim()
    );
    fs::write(dest.join("test_solution.py"), test).map_err(|e| e.to_string())?;

    let examples = extract_lcd_assert_examples(problem.test.trim());
    let mut body = if !problem.query.trim().is_empty() {
        problem.query.trim().to_string()
    } else {
        format!(
            "{}\n\nStarter:\n```python\n{}\n```\n",
            problem.problem_description.trim(),
            problem.starter_code.trim()
        )
    };
    let (reconciled_body, reconcile_note) = reconcile_lcd_spec_with_tests(&body, &examples);
    body = reconciled_body;
    let contract = build_lcd_test_contract(problem.test.trim(), problem.entry_point.trim());
    // Scoring contract first; reconciled writeup next (spec patched to match tests).
    let prompt = format!(
        "{contract}{reconcile_note}{body}\n\n---\nImplement the solution in `solution.py` only.\n\
Keep the `Solution` class and method signatures from the starter.\n\
Do NOT create, edit, or delete `test_solution.py` (or any `test_*.py`) — those files are the scoring harness and will be restored before grading.\n\
Do not add README or extra files unless required to make tests pass.\n\
Make `python3 -m unittest discover -s . -p 'test_*.py' -v` pass against the existing tests.\n\
Your solution must match every listed scoring example above (and the remaining hidden asserts).\n"
    );
    fs::write(dest.join("prompt.txt"), prompt).map_err(|e| e.to_string())?;

    let meta = serde_json::json!({
        "task_id": problem.task_id,
        "difficulty": problem.difficulty,
        "tags": problem.tags,
        "entry_point": problem.entry_point,
        "source": "newfacade/LeetCodeDataset",
    });
    fs::write(
        dest.join("meta.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn prepare_workspace(c: &Challenge, fixtures_coding: &Path, dest: &Path) -> Result<(), String> {
    if let Some(dir) = &c.hand_dir {
        copy_dir(&fixtures_coding.join(dir), dest).map_err(|e| e.to_string())
    } else if let Some(lcd) = &c.lcd {
        materialize_lcd(lcd, dest)
    } else {
        Err(format!("challenge {} has no payload", c.id))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let cli = parse_cli(&args);

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixtures_coding = repo_root.join("fixtures/coding");
    let fixtures_leetcode = repo_root.join("fixtures/leetcode");

    let mut challenges: Vec<Challenge> = Vec::new();
    let source = cli.source.to_lowercase();
    if source == "hand" || source == "all" {
        challenges.extend(hand_challenges());
    }
    if source == "lcd" || source == "leetcode" || source == "all" {
        match lcd_challenges(&fixtures_leetcode, &cli.lcd_version, &cli.split) {
            Ok(rows) => challenges.extend(rows),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    }
    if challenges.is_empty() {
        eprintln!("no challenges for --source {}", cli.source);
        eprintln!("use --source hand|lcd|all");
        return ExitCode::FAILURE;
    }

    if let Some(diff) = &cli.difficulty {
        let want: Vec<String> = diff.iter().map(|d| d.to_lowercase()).collect();
        challenges.retain(|c| match &c.lcd {
            Some(p) => want.iter().any(|d| p.difficulty.to_lowercase() == *d),
            None => false, // difficulty filter applies to LCD only
        });
    }

    if let Some(only) = &cli.only {
        challenges.retain(|c| only.iter().any(|id| id == &c.id));
    }

    if let Some(limit) = cli.limit {
        challenges.truncate(limit);
    }

    if cli.list {
        println!("count={}", challenges.len());
        for c in &challenges {
            let kind = match c.kind {
                Kind::Python => "python",
                Kind::Rust => "rust",
            };
            let src = if c.lcd.is_some() { "lcd" } else { "hand" };
            let diff = c.lcd.as_ref().map(|p| p.difficulty.as_str()).unwrap_or("-");
            println!("{}\t{}\t{}\t{}", c.id, kind, src, diff);
        }
        return ExitCode::SUCCESS;
    }

    if challenges.is_empty() {
        eprintln!("no challenges selected");
        return ExitCode::FAILURE;
    }

    if cli.check_stubs {
        let mut ok = true;
        for c in &challenges {
            let tmp = tempfile::tempdir().expect("tmpdir");
            if let Err(e) = prepare_workspace(c, &fixtures_coding, tmp.path()) {
                eprintln!("FAIL prepare {}: {e}", c.id);
                ok = false;
                continue;
            }
            match run_tests(tmp.path(), c.kind) {
                Ok(()) => {
                    eprintln!("FAIL stub unexpectedly passed tests: {}", c.id);
                    ok = false;
                }
                Err(_) => eprintln!("ok stub fails as expected: {}", c.id),
            }
        }
        return if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    let cfg = match VibeConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ollama = OllamaClient::new(&cfg);
    if !ollama.health().await {
        eprintln!("ollama not reachable at {}", cfg.ollama_url);
        return ExitCode::FAILURE;
    }
    let reason = ollama.resolve_reason_model(&cfg).await;
    let debug = ollama.resolve_debug_model(&cfg).await;
    println!("reason_model={reason}");
    match &debug {
        Some(d) => println!("debug_model={d}"),
        None if !cfg.debug_model.trim().is_empty() => {
            println!("debug_model=(unavailable: {})", cfg.debug_model.trim());
        }
        None => println!("debug_model=(off)"),
    }
    println!("agent_max_turns={}", cfg.agent_max_turns);
    println!("challenges={}", challenges.len());
    println!("source={} split={}", cli.source, cli.split);
    println!();

    let agent = AgentLoop {
        cfg: &cfg,
        ollama: &ollama,
        reason_model: &reason,
        debug_model: debug.as_deref(),
    };

    let mut results = Vec::new();
    for c in &challenges {
        println!("=== {} ===", c.id);
        let tmp = tempfile::tempdir().expect("tmpdir");
        if let Err(e) = prepare_workspace(c, &fixtures_coding, tmp.path()) {
            eprintln!("prepare failed: {e}");
            results.push(CaseResult {
                id: c.id.clone(),
                passed: false,
                seconds: 0.0,
                summary: e,
                files: vec![],
                test_output: String::new(),
                difficulty: c
                    .lcd
                    .as_ref()
                    .map(|p| p.difficulty.clone())
                    .unwrap_or_default(),
                source: if c.lcd.is_some() { "lcd" } else { "hand" }.into(),
            });
            continue;
        }
        let root = tmp.path().to_path_buf();

        if run_tests(&root, c.kind).is_ok() {
            eprintln!("warning: stub already passes for {}", c.id);
        }

        let prompt = fs::read_to_string(root.join("prompt.txt")).unwrap_or_else(|_| {
            format!(
                "Solve the coding challenge in this workspace so tests pass. Challenge id: {}",
                c.id
            )
        });

        let started = Instant::now();
        let shadow = match ShadowWorkspace::new(&root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("shadow init failed: {e}");
                results.push(CaseResult {
                    id: c.id.clone(),
                    passed: false,
                    seconds: 0.0,
                    summary: e.to_string(),
                    files: vec![],
                    test_output: String::new(),
                    difficulty: c
                        .lcd
                        .as_ref()
                        .map(|p| p.difficulty.clone())
                        .unwrap_or_default(),
                    source: if c.lcd.is_some() { "lcd" } else { "hand" }.into(),
                });
                continue;
            }
        };
        let _ = shadow.sync_from_origin();

        let fallback = match c.kind {
            Kind::Python => "solution.py",
            Kind::Rust => "src/lib.rs",
        };
        let heal = match agent.run(&prompt, &root, &shadow, fallback, false).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("agent error: {e}");
                results.push(CaseResult {
                    id: c.id.clone(),
                    passed: false,
                    seconds: started.elapsed().as_secs_f64(),
                    summary: e.to_string(),
                    files: vec![],
                    test_output: String::new(),
                    difficulty: c
                        .lcd
                        .as_ref()
                        .map(|p| p.difficulty.clone())
                        .unwrap_or_default(),
                    source: if c.lcd.is_some() { "lcd" } else { "hand" }.into(),
                });
                continue;
            }
        };

        for f in &heal.files {
            // Never let the agent replace the hidden scoring tests.
            if is_protected_test_path(&f.path) {
                eprintln!("skip apply (protected test): {}", f.path);
                continue;
            }
            if let Err(e) = shadow.apply_to_origin(&f.path, &f.content) {
                eprintln!("apply {}: {e}", f.path);
            }
        }
        // Re-materialize canonical tests in case anything slipped through.
        if let Err(e) = restore_tests(c, &fixtures_coding, &root) {
            eprintln!("restore tests failed: {e}");
        }

        let elapsed = started.elapsed().as_secs_f64();
        let test_res = run_tests(&root, c.kind);
        let (passed, test_output) = match test_res {
            Ok(()) => (true, "tests passed".into()),
            Err(out) => (false, out),
        };

        let files: Vec<String> = heal.files.iter().map(|f| f.path.clone()).collect();
        let summary = heal
            .summary
            .clone()
            .unwrap_or_else(|| format!("success={} files={}", heal.success, files.len()));

        println!(
            "{} in {:.1}s — {}",
            if passed { "PASS" } else { "FAIL" },
            elapsed,
            files.join(", ")
        );
        if let Some(s) = &heal.summary {
            println!("summary: {}", s.chars().take(240).collect::<String>());
        }
        if !passed {
            println!("tests:\n{}", truncate(&test_output, 800));
        }
        println!();

        results.push(CaseResult {
            id: c.id.clone(),
            passed,
            seconds: elapsed,
            summary,
            files,
            test_output,
            difficulty: c
                .lcd
                .as_ref()
                .map(|p| p.difficulty.clone())
                .unwrap_or_default(),
            source: if c.lcd.is_some() { "lcd" } else { "hand" }.into(),
        });
    }

    let passed = results.iter().filter(|r| r.passed).count();
    let total = results.len();
    println!("==============================");
    println!("SCORE: {passed}/{total}");
    for r in &results {
        println!(
            "  {} {} ({:.1}s) [{}{}]",
            if r.passed { "PASS" } else { "FAIL" },
            r.id,
            r.seconds,
            r.source,
            if r.difficulty.is_empty() {
                String::new()
            } else {
                format!("/{}", r.difficulty)
            }
        );
    }

    if let Some(path) = cli.json_out {
        let payload = serde_json::json!({
            "model": reason,
            "passed": passed,
            "total": total,
            "source": cli.source,
            "split": cli.split,
            "results": results.iter().map(|r| serde_json::json!({
                "id": r.id,
                "passed": r.passed,
                "seconds": r.seconds,
                "summary": r.summary,
                "files": r.files,
                "test_output": r.test_output,
                "difficulty": r.difficulty,
                "source": r.source,
            })).collect::<Vec<_>>(),
        });
        if let Err(e) = fs::write(&path, serde_json::to_string_pretty(&payload).unwrap()) {
            eprintln!("write json failed: {e}");
        } else {
            println!("wrote {path}");
        }
    }

    if passed == total {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn is_protected_test_path(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    (name.starts_with("test_") && name.ends_with(".py")) || name == "test_solution.py"
}

fn python_string_literal(s: &str) -> String {
    // Prefer JSON-style double quotes — valid Python string literal.
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

/// Method name from LCD `entry_point` like `Solution().hasMatch` → `hasMatch`.
fn lcd_entry_method(entry_point: &str) -> &str {
    entry_point
        .rsplit('.')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("candidate")
}

/// `(call_args, expected)` pairs from HumanEval-style `assert candidate(...) == …`.
fn extract_lcd_assert_examples(test: &str) -> Vec<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*assert\s+candidate\((.+)\)\s*==\s*(.+?)\s*$")
            .expect("lcd example assert regex")
    });
    re.captures_iter(test)
        .filter_map(|c| {
            Some((
                c.get(1)?.as_str().trim().to_string(),
                c.get(2)?.as_str().trim().to_string(),
            ))
        })
        .collect()
}

/// Prefer a mix of distinct expected values (e.g. True and False), then fill by order.
fn select_balanced_lcd_examples(
    examples: &[(String, String)],
    max: usize,
) -> Vec<(String, String)> {
    if examples.len() <= max {
        return examples.to_vec();
    }
    use std::collections::BTreeMap;
    let mut by_exp: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, (_, exp)) in examples.iter().enumerate() {
        by_exp.entry(exp.as_str()).or_default().push(i);
    }
    let mut chosen = Vec::new();
    let mut chosen_idx = std::collections::BTreeSet::new();
    // Round-robin across expected-value buckets so one label cannot dominate the cap.
    let mut cursors: BTreeMap<&str, usize> = by_exp.keys().map(|k| (*k, 0usize)).collect();
    while chosen.len() < max {
        let mut progress = false;
        let keys: Vec<&str> = by_exp.keys().copied().collect();
        for k in keys {
            if chosen.len() >= max {
                break;
            }
            let idxs = &by_exp[k];
            let c = cursors.get_mut(k).unwrap();
            if *c < idxs.len() {
                let i = idxs[*c];
                *c += 1;
                if chosen_idx.insert(i) {
                    chosen.push(examples[i].clone());
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
    }
    // Preserve original assert order for readability.
    chosen.sort_by_key(|(args, exp)| {
        examples
            .iter()
            .position(|(a, e)| a == args && e == exp)
            .unwrap_or(usize::MAX)
    });
    chosen
}

/// String / bytes literals appearing in `candidate(...)` call args.
fn extract_python_string_literals(args: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?:"((?:\\.|[^"\\])*)"|'((?:\\.|[^'\\])*)')"#).expect("py string lit")
    });
    re.captures_iter(args)
        .filter_map(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().replace("\\\"", "\"").replace("\\'", "'"))
        })
        .collect()
}

fn parse_count_word(word: &str) -> Option<usize> {
    match word.to_ascii_lowercase().as_str() {
        "one" | "a" | "an" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "zero" => Some(0),
        other => other.parse().ok(),
    }
}

#[derive(Debug, Clone)]
struct ExactCountClaim {
    /// Inclusive char span in the original description for the claim sentence fragment.
    span: (usize, usize),
    /// Exact count claimed by the writeup.
    claimed: usize,
    /// Symbol the writeup constrains (e.g. `*`).
    symbol: String,
}

/// Find "exactly N '<sym>'" style claims in the problem writeup.
fn find_exact_count_claims(description: &str) -> Vec<ExactCountClaim> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?i)exactly\s+(one|two|three|four|five|zero|\d+)\s+(?:['\"]([^'\"]+)['\"]|`([^`]+)`)"#,
        )
        .expect("exact count claim")
    });
    re.captures_iter(description)
        .filter_map(|c| {
            let full = c.get(0)?;
            let claimed = parse_count_word(c.get(1)?.as_str())?;
            let symbol = c
                .get(2)
                .or_else(|| c.get(3))
                .map(|m| m.as_str().to_string())?;
            if symbol.is_empty() {
                return None;
            }
            Some(ExactCountClaim {
                span: (full.start(), full.end()),
                claimed,
                symbol,
            })
        })
        .collect()
}

/// Max occurrences of `symbol` in any single string literal of a call (across examples).
fn observed_symbol_counts(examples: &[(String, String)], symbol: &str) -> Option<(usize, usize)> {
    if symbol.is_empty() {
        return None;
    }
    let mut min_c = usize::MAX;
    let mut max_c = 0usize;
    let mut any = false;
    for (args, _) in examples {
        for lit in extract_python_string_literals(args) {
            // Multi-char symbols: count non-overlapping occurrences.
            let count = if symbol.chars().count() == 1 {
                let ch = symbol.chars().next().unwrap();
                lit.chars().filter(|c| *c == ch).count()
            } else {
                lit.matches(symbol).count()
            };
            // Only track literals that mention the symbol at least once (the constrained field).
            if count == 0 {
                continue;
            }
            any = true;
            min_c = min_c.min(count);
            max_c = max_c.max(count);
        }
    }
    if any {
        Some((min_c, max_c))
    } else {
        None
    }
}

/// When writeup claims conflict with scoring asserts, rewrite those claims to match the tests.
fn reconcile_lcd_spec_with_tests(
    description: &str,
    examples: &[(String, String)],
) -> (String, String) {
    let claims = find_exact_count_claims(description);
    if claims.is_empty() {
        // Soften "guaranteed" prose when tests expect ±inf sentinels.
        let has_inf = examples.iter().any(|(_, e)| {
            e.trim() == "inf"
                || e.trim() == "-inf"
                || e.contains("float(\"inf\")")
                || e.contains("float('inf')")
                || e.contains("-inf")
        });
        let desc_l = description.to_lowercase();
        if has_inf && (desc_l.contains("guaranteed") || desc_l.contains("at least one")) {
            let mut fixed = description.to_string();
            for pat in [
                "guaranteed",
                "Guaranteed",
                "at least one character with an odd frequency and one with an even frequency",
            ] {
                if let Some(i) = fixed.find(pat) {
                    let note =
                        " [tests may include edges outside this claim — follow scoring examples] ";
                    fixed.insert_str(i + pat.len(), note);
                    break;
                }
            }
            let note = String::from(
                "### Spec reconciled with tests\n\
Detected possible prose/test mismatch (sentinel expects vs \"guaranteed\" constraints). \
Treat scoring examples as definitive; soft-noted the conflicting claim in the writeup below.\n\n",
            );
            return (fixed, note);
        }
        return (description.to_string(), String::new());
    }

    let mut edits: Vec<(usize, usize, String, String)> = Vec::new(); // start,end,replacement,summary
    for claim in &claims {
        let Some((min_c, max_c)) = observed_symbol_counts(examples, &claim.symbol) else {
            continue;
        };
        if min_c == claim.claimed && max_c == claim.claimed {
            continue; // tests agree with writeup
        }
        let sym_disp = claim.symbol.replace('`', "'");
        let replacement = format!(
            "between {min_c} and {max_c} '{sym_disp}' (tests; was exactly {})",
            claim.claimed
        );
        let summary = format!(
            "- Writeup claimed exactly {} × '{sym_disp}', but scoring tests use {min_c}..{max_c} in inputs.",
            claim.claimed
        );
        edits.push((claim.span.0, claim.span.1, replacement, summary));
    }

    if edits.is_empty() {
        return (description.to_string(), String::new());
    }

    // Apply from the end so spans stay valid.
    edits.sort_by_key(|(s, _, _, _)| *s);
    edits.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let mut fixed = description.to_string();
    let mut summaries = Vec::new();
    for (start, end, replacement, summary) in edits.into_iter().rev() {
        if start < end && end <= fixed.len() {
            fixed.replace_range(start..end, &replacement);
            summaries.push(summary);
        }
    }
    summaries.reverse();
    let note = format!(
        "### Spec reconciled with tests\n\
The problem writeup disagreed with the scoring tests. Conflicting constraints were rewritten from test evidence:\n{}\n\
Use the reconciled writeup + scoring examples; ignore any leftover contradicting prose.\n\n",
        summaries.join("\n")
    );
    (fixed, note)
}

/// Generic override + capped concrete I/O from scoring asserts (no feature-specific rules).
fn build_lcd_test_contract(test: &str, entry_point: &str) -> String {
    const MAX_EXAMPLES: usize = 12;
    const MAX_CHARS: usize = 1200;

    let method = lcd_entry_method(entry_point);
    let examples = extract_lcd_assert_examples(test);
    if examples.is_empty() {
        return String::from(
            "### Scoring contract (authoritative)\n\
The unit tests are the only source of truth for required behavior. \
Do not trust constraints or claims in the problem writeup that disagree with the tests — implement what the tests require.\n\
Checklist: make `python3 -m unittest` pass; if any assert fails, change the approach.\n\n",
        );
    }

    let selected = select_balanced_lcd_examples(&examples, MAX_EXAMPLES);
    let mut lines = Vec::new();
    let mut used = 0usize;
    for (args, exp) in &selected {
        let line = format!("- `{method}({args})` → `{exp}`");
        if used + line.len() + 1 > MAX_CHARS && !lines.is_empty() {
            break;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    let omitted = examples.len().saturating_sub(lines.len());

    let mut out = String::from(
        "### Scoring examples (authoritative — read this first)\n\
The unit tests and the examples below are the only source of truth for required behavior. \
Do not trust constraints or claims in the problem writeup that disagree with these examples or the tests — implement what the examples require.\n\
Checklist: a correct solution returns the arrow value for EVERY listed example; if any one fails, discard the approach and try another. Hidden asserts in the test file must also pass.\n",
    );
    out.push_str(&lines.join("\n"));
    out.push('\n');
    if omitted > 0 {
        out.push_str(&format!(
            "…and {omitted} more assert(s) in the scoring tests (not shown).\n"
        ));
    }
    out.push('\n');
    out
}

/// Rewrite single-line `assert candidate(...) == expected` into got/expected errors.
fn enrich_lcd_check_asserts(test: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?m)^(?P<indent>\s*)assert\s+(?P<call>candidate\(.+\))\s*==\s*(?P<exp>.+?)\s*$",
        )
        .expect("lcd assert regex")
    });
    re.replace_all(test, |caps: &regex::Captures| {
        let ind = caps.name("indent").map(|m| m.as_str()).unwrap_or("");
        let call = caps.name("call").map(|m| m.as_str()).unwrap_or("candidate()");
        let exp = caps.name("exp").map(|m| m.as_str()).unwrap_or("None");
        // Keep the call text in a separate string so AssertionError messages
        // still name the failing case (traceback often only shows the raise).
        let call_lit = python_string_literal(call);
        format!(
            "{ind}_call = {call_lit}\n{ind}_got = {call}\n{ind}_exp = {exp}\n{ind}if _got != _exp:\n{ind}    raise AssertionError(f\"{{_call}} got {{_got!r}} expected {{_exp!r}}\")"
        )
    })
    .into_owned()
}

fn restore_tests(c: &Challenge, fixtures_coding: &Path, root: &Path) -> Result<(), String> {
    if let Some(lcd) = &c.lcd {
        let test = format!(
            r#"import unittest

from solution import Solution

inf = float("inf")
nan = float("nan")

{test}

class TestLCD(unittest.TestCase):
    def test_all(self):
        check({entry})


if __name__ == "__main__":
    unittest.main()
"#,
            test = enrich_lcd_check_asserts(lcd.test.trim()),
            entry = lcd.entry_point.trim()
        );
        fs::write(root.join("test_solution.py"), test).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if let Some(dir) = &c.hand_dir {
        let src = fixtures_coding.join(dir);
        for entry in fs::read_dir(&src).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("test_") && name_str.ends_with(".py") {
                fs::copy(entry.path(), root.join(&name)).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

fn run_tests(root: &Path, kind: Kind) -> Result<(), String> {
    use std::io::Read;
    // Agent solutions can infinite-loop; bound wall time.
    let timeout = match kind {
        Kind::Python => Duration::from_secs(45),
        Kind::Rust => Duration::from_secs(120),
    };
    let mut child = match kind {
        Kind::Python => Command::new("python3")
            .args([
                "-m",
                "unittest",
                "discover",
                "-s",
                ".",
                "-p",
                "test_*.py",
                "-v",
            ])
            .current_dir(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?,
        Kind::Rust => Command::new("cargo")
            .args(["test", "--quiet"])
            .current_dir(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?,
    };
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "missing stdout".to_string())?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "missing stderr".to_string())?;
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut out);
        let _ = stderr_pipe.read_to_end(&mut err);
        (out, err)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(format!("test timeout after {}s", timeout.as_secs()));
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let (out, err) = reader
        .join()
        .map_err(|_| "stdout/stderr reader panicked".to_string())?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    if status.success() {
        Ok(())
    } else {
        Err(combined)
    }
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_assert_examples() {
        let test = r#"
def check(candidate):
    assert candidate(s = "abcdefg",p = "a*d*g") == True
    assert candidate(s = "pqrstuvwabcdef") == -inf
"#;
        let ex = extract_lcd_assert_examples(test);
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].0, "s = \"abcdefg\",p = \"a*d*g\"");
        assert_eq!(ex[0].1, "True");
        assert_eq!(ex[1].1, "-inf");
    }

    #[test]
    fn contract_is_generic_and_capped() {
        let mut test = String::from("def check(candidate):\n");
        for i in 0..20 {
            test.push_str(&format!("    assert candidate(s = \"x{i}\") == {i}\n"));
        }
        let c = build_lcd_test_contract(&test, "Solution().maxDifference");
        assert!(c.contains("source of truth") || c.contains("authoritative"));
        assert!(c.contains("read this first"));
        assert!(c.contains("EVERY listed example"));
        assert!(c.contains("maxDifference("));
        assert!(c.contains("→"));
        // Cap at 12 examples
        let shown = c.lines().filter(|l| l.starts_with("- `")).count();
        assert_eq!(shown, 12);
        assert!(c.contains("more assert"));
        // No feature-specific rule language
        assert!(!c.to_lowercase().contains("wildcard"));
        assert!(!c.to_lowercase().contains("exactly one"));
    }

    #[test]
    fn balanced_sample_includes_both_bool_labels() {
        let mut test = String::from("def check(candidate):\n");
        for i in 0..15 {
            test.push_str(&format!("    assert candidate(s = \"t{i}\") == True\n"));
        }
        test.push_str("    assert candidate(s = \"f0\") == False\n");
        test.push_str("    assert candidate(s = \"f1\") == False\n");
        let selected = select_balanced_lcd_examples(&extract_lcd_assert_examples(&test), 12);
        let has_true = selected.iter().any(|(_, e)| e == "True");
        let has_false = selected.iter().any(|(_, e)| e == "False");
        assert!(has_true && has_false, "{selected:?}");
        assert_eq!(selected.len(), 12);
    }

    #[test]
    fn entry_method_parse() {
        assert_eq!(lcd_entry_method("Solution().hasMatch"), "hasMatch");
        assert_eq!(
            lcd_entry_method("Solution().maxDifference"),
            "maxDifference"
        );
    }

    #[test]
    fn reconciles_exact_count_when_tests_diverge() {
        let desc = "You are given p, where p contains exactly one '*' character.\n\
Constraints:\n\
p contains exactly one '*'";
        let test = r#"
def check(candidate):
    assert candidate(s = "abcdefg",p = "a*d*g") == True
    assert candidate(s = "xy",p = "*") == True
    assert candidate(s = "car",p = "c*v") == False
"#;
        let examples = extract_lcd_assert_examples(test);
        let (fixed, note) = reconcile_lcd_spec_with_tests(desc, &examples);
        assert!(note.contains("Spec reconciled"), "{note}");
        assert!(fixed.contains("between 1 and 2"), "{fixed}");
        assert!(!fixed.contains("exactly one '*'"), "{fixed}");
    }
}

//! Multi-prompt scenario runner for vibe agent (prompt-by-prompt project builds).
//!
//! Example (remote B2B job aggregator, ~100 prompts):
//!   cargo run -p vibe_core --bin vibe_scenario -- --list
//!   cargo run -p vibe_core --bin vibe_scenario -- --from 1 --to 10
//!   cargo run -p vibe_core --bin vibe_scenario -- --from 1 --to 100 --json /tmp/out.json
//!   cargo run -p vibe_core --bin vibe_scenario -- --workspace /tmp/ws --check-only

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;
use vibe_core::agent::{collect_test_snapshot, ensure_origin_test_snapshot, AgentLoop};
use vibe_core::config::VibeConfig;
use vibe_core::entrypoint::{
    cli_help_instruction, detect_python_cli_module, has_python_package, suggest_fallback_path,
    synthesize_constraints_md,
};
use vibe_core::ollama::OllamaClient;
use vibe_core::shadow::ShadowWorkspace;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ScenarioPrompt {
    id: u32,
    phase: String,
    prompt: String,
    #[serde(default)]
    checkpoint: Option<String>,
}

#[derive(Debug, Serialize)]
struct StepResult {
    id: u32,
    phase: String,
    ok: bool,
    seconds: f64,
    files: Vec<String>,
    checkpoint: Option<String>,
    checkpoint_ok: Option<bool>,
    summary: String,
    error: Option<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scenario_dir(name: &str) -> PathBuf {
    repo_root().join("fixtures/scenarios").join(name)
}

fn load_prompts(path: &Path) -> Result<Vec<ScenarioPrompt>, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: ScenarioPrompt =
            serde_json::from_str(line).map_err(|e| format!("prompts.jsonl line {}: {e}", i + 1))?;
        out.push(row);
    }
    out.sort_by_key(|p| p.id);
    Ok(out)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn run_checkpoint(scenario: &Path, workspace: &Path, name: &str) -> Result<(), String> {
    let script = scenario.join("checkpoints/run_checks.py");
    let spec = scenario.join("checkpoints.json");
    if !script.exists() {
        return Err(format!("missing {}", script.display()));
    }
    let output = Command::new("python3")
        .arg(&script)
        .arg("--workspace")
        .arg(workspace)
        .arg("--checkpoint")
        .arg(name)
        .arg("--spec")
        .arg(&spec)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = format!("{stdout}{stderr}");
        let detail = detail.trim();
        if detail.is_empty() {
            Err(format!("checkpoint `{name}` failed"))
        } else {
            Err(format!(
                "checkpoint `{name}` failed:\n{}",
                truncate_str(detail, 2500)
            ))
        }
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

fn trim_constraints_body(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Load `constraints.md`, or synthesize + write it when missing.
fn ensure_scenario_constraints(scenario: &Path, work: &Path, sample_prompt: &str) -> String {
    let path = scenario.join("constraints.md");
    if path.is_file() {
        return trim_constraints_body(&fs::read_to_string(&path).unwrap_or_default());
    }
    let body = synthesize_constraints_md(work, sample_prompt);
    match fs::write(&path, &body) {
        Ok(()) => eprintln!(
            "[vibe] created missing {} (synthesized from workspace/prompt)",
            path.display()
        ),
        Err(e) => eprintln!(
            "[vibe] could not write {}: {e} — using synthesized constraints in-memory",
            path.display()
        ),
    }
    trim_constraints_body(&body)
}

fn build_step_goal(scenario_constraints: &str, work: &Path, step_prompt: &str) -> String {
    let cli = detect_python_cli_module(work, &BTreeMap::new(), step_prompt);
    let help = cli_help_instruction(cli.as_ref());
    let mut parts = Vec::new();
    parts.push(step_prompt.trim().to_string());
    parts.push("\n---\nScenario constraints:".into());
    if !scenario_constraints.is_empty() {
        parts.push(scenario_constraints.to_string());
    }
    parts.push(
        "- Keep prior files; extend rather than rewrite from scratch unless broken.\n\
- Do NOT delete or empty `tests/` or `test_*.py` files — the test suite must remain.\n\
- Avoid duplicate modules (e.g. do not add `filter.py` if `filters.py` exists).\n\
- Prefer extending existing modules over creating parallel ones with the same job."
            .into(),
    );
    parts.push(format!(
        "- {help} after install / without crashing on import.\n\
- Lazy-import optional heavy deps inside functions that need them — not at module top-level for CLI help."
    ));
    parts.join("\n")
}

fn build_heal_goal(cp: &str, work: &Path, last_err: &str) -> String {
    let cli = detect_python_cli_module(work, &BTreeMap::new(), last_err);
    let help = cli_help_instruction(cli.as_ref());
    format!(
        "Checkpoint `{cp}` FAILED. Fix the workspace until this checkpoint passes.\n\
Do NOT delete tests. Do NOT create duplicate modules.\n\
\n\
Hard requirements:\n\
1. {help} (lazy-import optional heavy deps — not at top level).\n\
2. Read the failure traceback and fix THAT error — create missing modules or remove bad imports.\n\
3. If ModuleNotFoundError names a third-party package, add it to pyproject.toml/requirements.txt OR stop importing it at CLI import time.\n\
4. No empty stub files for modules the checkpoint checks.\n\
5. Do not oscillate the same import (missing ↔ duplicate) across turns.\n\
6. If failure says help_contains / -h contains 'NAME', that exact NAME must appear in CLI help (do not rename synonyms).\n\
Unit tests alone are NOT enough for DONE.\n\n\
### Checkpoint failure output:\n{last_err}\n"
    )
}

fn skip_heal_snap_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | ".git"
            | ".vibe"
            | "dist"
            | "build"
            | ".venv"
            | "__pycache__"
            | ".eggs"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
    ) || name.ends_with(".egg-info")
}

/// Snapshot editable source files (excludes venv / build caches) for heal A/B restore.
fn snapshot_origin_sources(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if skip_heal_snap_dir(&name) {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
                continue;
            }
            if !path.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if let Ok(bytes) = fs::read(&path) {
                out.insert(rel, bytes);
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn restore_origin_sources(root: &Path, snap: &BTreeMap<String, Vec<u8>>) {
    let current = snapshot_origin_sources(root);
    for key in current.keys() {
        if snap.contains_key(key) {
            continue;
        }
        let path = root.join(key);
        let _ = fs::remove_file(&path);
    }
    for (rel, bytes) in snap {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, bytes);
    }
}

#[derive(Debug, Clone)]
struct HealBranchResult {
    label: &'static str,
    temperature: f32,
    ok: bool,
    err: String,
    files: Vec<(String, String)>,
}

fn checkpoint_pass_count(err: &str) -> usize {
    err.lines()
        .filter(|l| l.starts_with("PASS ") || l.starts_with("PASS\t"))
        .count()
}

/// Pick A/B heal winner: prefer checkpoint pass; if both pass prefer cooler;
/// if neither, prefer more PASS lines then shorter residual error; ties → cooler.
fn pick_heal_ab_winner(a: &HealBranchResult, b: &HealBranchResult) -> usize {
    match (a.ok, b.ok) {
        (true, false) => 0,
        (false, true) => 1,
        (true, true) => {
            if b.temperature < a.temperature {
                1
            } else {
                0
            }
        }
        (false, false) => {
            let pa = checkpoint_pass_count(&a.err);
            let pb = checkpoint_pass_count(&b.err);
            if pb != pa {
                if pb > pa {
                    1
                } else {
                    0
                }
            } else if b.err.len() != a.err.len() {
                if b.err.len() < a.err.len() {
                    1
                } else {
                    0
                }
            } else if b.temperature < a.temperature {
                1
            } else {
                0
            }
        }
    }
}

async fn run_heal_branch(
    label: &'static str,
    temperature: f32,
    scen: &Path,
    work: &Path,
    cp: &str,
    heal_goal: &str,
    agent: &AgentLoop<'_>,
    ollama: &OllamaClient,
    fallback: &str,
    baseline: &BTreeMap<String, Vec<u8>>,
    test_snap_heal: &BTreeMap<String, String>,
    test_snap: &BTreeMap<String, String>,
) -> HealBranchResult {
    restore_origin_sources(work, baseline);
    ensure_origin_test_snapshot(work, test_snap_heal);
    ensure_origin_test_snapshot(work, test_snap);
    let heal_shadow = match ShadowWorkspace::new(work) {
        Ok(s) => s,
        Err(err) => {
            return HealBranchResult {
                label,
                temperature,
                ok: false,
                err: format!("shadow init (heal): {err}"),
                files: vec![],
            };
        }
    };
    let _ = heal_shadow.sync_from_origin();
    let _temp_guard = ollama.temperature_override_guard(temperature);
    eprintln!("→ heal branch {label} temperature={temperature}…");
    let branch_files = match agent
        .run(heal_goal, work, &heal_shadow, fallback, false)
        .await
    {
        Ok(h2) => {
            let mut out = Vec::new();
            for f in &h2.files {
                let _ = heal_shadow.apply_to_origin(&f.path, &f.content);
                out.push((f.path.clone(), f.content.clone()));
            }
            ensure_origin_test_snapshot(work, test_snap_heal);
            ensure_origin_test_snapshot(work, test_snap);
            out
        }
        Err(err) => {
            return HealBranchResult {
                label,
                temperature,
                ok: false,
                err: format!("checkpoint heal agent error: {err:#}"),
                files: vec![],
            };
        }
    };
    match run_checkpoint(scen, work, cp) {
        Ok(()) => HealBranchResult {
            label,
            temperature,
            ok: true,
            err: String::new(),
            files: branch_files,
        },
        Err(err2) => HealBranchResult {
            label,
            temperature,
            ok: false,
            err: err2,
            files: branch_files,
        },
    }
}

fn parse_args() -> Cli {
    let args: Vec<String> = env::args().collect();
    let mut cli = Cli::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--scenario" => {
                i += 1;
                cli.scenario = args.get(i).cloned().unwrap_or(cli.scenario.clone());
            }
            "--from" => {
                i += 1;
                cli.from = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1);
            }
            "--to" => {
                i += 1;
                cli.to = args.get(i).and_then(|s| s.parse().ok());
            }
            "--workspace" => {
                i += 1;
                cli.workspace = args.get(i).map(PathBuf::from);
            }
            "--json" => {
                i += 1;
                cli.json = args.get(i).map(PathBuf::from);
            }
            "--list" => cli.list = true,
            "--check-only" => cli.check_only = true,
            "--stop-on-checkpoint-fail" => cli.stop_on_checkpoint_fail = true,
            "--continue-on-checkpoint-fail" => cli.stop_on_checkpoint_fail = false,
            "--checkpoint-heal-max" => {
                i += 1;
                cli.checkpoint_heal_max = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(cli.checkpoint_heal_max);
            }
            "--help" | "-h" => cli.help = true,
            other => eprintln!("unknown arg: {other}"),
        }
        i += 1;
    }
    cli
}

#[derive(Debug)]
struct Cli {
    scenario: String,
    from: u32,
    to: Option<u32>,
    workspace: Option<PathBuf>,
    json: Option<PathBuf>,
    list: bool,
    check_only: bool,
    stop_on_checkpoint_fail: bool,
    /// Repair attempts after a failed checkpoint (0 disables).
    checkpoint_heal_max: u32,
    help: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            scenario: "remote_b2b_jobs".into(),
            from: 1,
            to: None,
            workspace: None,
            json: None,
            list: false,
            check_only: false,
            stop_on_checkpoint_fail: true,
            checkpoint_heal_max: 3,
            help: false,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = parse_args();
    if cli.help {
        eprintln!(
            "vibe_scenario — run multi-prompt agent scenarios\n\
             --scenario NAME     (default remote_b2b_jobs)\n\
             --from N --to M     prompt id range (inclusive)\n\
             --workspace PATH    reuse/write workdir (default: temp copy of fixture workspace)\n\
             --list              print prompts and exit\n\
             --check-only        run all checkpoints against --workspace\n\
             --json PATH         write results JSON\n\
             --stop-on-checkpoint-fail (default true)\n\
             --continue-on-checkpoint-fail  keep going after a failed checkpoint\n\
             --checkpoint-heal-max N  repair turns after FAIL (default 3; 0=off)"
        );
        return ExitCode::SUCCESS;
    }

    let scen = scenario_dir(&cli.scenario);
    if !scen.is_dir() {
        eprintln!("scenario not found: {}", scen.display());
        return ExitCode::FAILURE;
    }
    let prompts_path = scen.join("prompts.jsonl");
    let prompts = match load_prompts(&prompts_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if prompts.is_empty() {
        eprintln!("no prompts in {}", prompts_path.display());
        return ExitCode::FAILURE;
    }

    if cli.list {
        println!("scenario={} prompts={}", cli.scenario, prompts.len());
        for p in &prompts {
            let cp = p.checkpoint.as_deref().unwrap_or("-");
            println!(
                "{:3} [{:<10}] checkpoint={:<10} {}",
                p.id,
                p.phase,
                cp,
                p.prompt
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(90)
                    .collect::<String>()
            );
        }
        return ExitCode::SUCCESS;
    }

    let work = if let Some(ws) = &cli.workspace {
        fs::create_dir_all(ws).ok();
        // Seed golden tests if the workspace was created empty / hollowed out.
        let starter = scen.join("workspace");
        if starter.is_dir() {
            let golden_tests = starter.join("tests");
            let dest_tests = ws.join("tests");
            if golden_tests.is_dir()
                && (!dest_tests.is_dir() || collect_test_snapshot(ws).is_empty())
            {
                let _ = copy_dir(&golden_tests, &dest_tests);
                eprintln!("seeded golden tests/ into workspace");
            }
        }
        ws.clone()
    } else {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("workspace");
        let starter = scen.join("workspace");
        if starter.is_dir() {
            if let Err(e) = copy_dir(&starter, &path) {
                eprintln!("copy starter: {e}");
                return ExitCode::FAILURE;
            }
        } else {
            fs::create_dir_all(&path).ok();
        }
        // Leak tempdir for process lifetime when no --workspace given.
        std::mem::forget(tmp);
        path
    };

    println!("scenario={}", cli.scenario);
    println!("workspace={}", work.display());
    println!("prompts_available={}", prompts.len());

    if cli.check_only {
        let spec: serde_json::Value = match fs::read_to_string(scen.join("checkpoints.json")) {
            Ok(t) => serde_json::from_str(&t).unwrap_or(json!({})),
            Err(e) => {
                eprintln!("checkpoints.json: {e}");
                return ExitCode::FAILURE;
            }
        };
        let mut failed = 0;
        if let Some(obj) = spec.as_object() {
            for name in obj.keys() {
                print!("checkpoint {name}… ");
                match run_checkpoint(&scen, &work, name) {
                    Ok(()) => println!("ok"),
                    Err(e) => {
                        println!("FAIL {e}");
                        failed += 1;
                    }
                }
            }
        }
        return if failed == 0 {
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
    if let Some(ref d) = debug {
        println!("debug_model={d}");
    }

    let agent = AgentLoop {
        cfg: &cfg,
        ollama: &ollama,
        reason_model: &reason,
        debug_model: debug.as_deref(),
    };

    let to = cli.to.unwrap_or(prompts.last().map(|p| p.id).unwrap_or(1));
    let selected: Vec<_> = prompts
        .into_iter()
        .filter(|p| p.id >= cli.from && p.id <= to)
        .collect();
    println!(
        "running {} prompt(s) ({}..={})",
        selected.len(),
        cli.from,
        to
    );

    let sample = selected.first().map(|p| p.prompt.as_str()).unwrap_or("");
    let scenario_constraints = ensure_scenario_constraints(&scen, &work, sample);

    let mut results = Vec::new();
    let mut hard_fail = false;

    for (idx, step) in selected.iter().enumerate() {
        println!(
            "\n=== prompt {}/{} id={} phase={} ===",
            idx + 1,
            selected.len(),
            step.id,
            step.phase
        );
        let started = Instant::now();
        let shadow = match ShadowWorkspace::new(&work) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("shadow init: {e}");
                hard_fail = true;
                break;
            }
        };
        let _ = shadow.sync_from_origin();
        // First prompts in an empty tree: greenfield. Later: edit existing project.
        let greenfield = !has_python_package(&work) && step.id <= 3;
        let fallback = suggest_fallback_path(&work);
        let goal = build_step_goal(&scenario_constraints, &work, &step.prompt);

        let test_snap = collect_test_snapshot(&work);

        let heal = match agent
            .run(&goal, &work, &shadow, &fallback, greenfield)
            .await
        {
            Ok(h) => h,
            Err(e) => {
                eprintln!("agent error: {e:#}");
                results.push(StepResult {
                    id: step.id,
                    phase: step.phase.clone(),
                    ok: false,
                    seconds: started.elapsed().as_secs_f64(),
                    files: vec![],
                    checkpoint: step.checkpoint.clone(),
                    checkpoint_ok: None,
                    summary: String::new(),
                    error: Some(format!("{e:#}")),
                });
                continue;
            }
        };

        for f in &heal.files {
            if let Err(e) = shadow.apply_to_origin(&f.path, &f.content) {
                eprintln!("apply {}: {e}", f.path);
            }
        }
        // Never leave the origin without the tests that existed at step start.
        ensure_origin_test_snapshot(&work, &test_snap);
        let mut files: Vec<String> = heal.files.iter().map(|f| f.path.clone()).collect();
        let mut checkpoint_ok = None;
        let mut checkpoint_err: Option<String> = None;
        if let Some(cp) = &step.checkpoint {
            print!("→ checkpoint `{cp}`… ");
            match run_checkpoint(&scen, &work, cp) {
                Ok(()) => {
                    println!("ok");
                    checkpoint_ok = Some(true);
                }
                Err(e) => {
                    println!("FAIL");
                    eprintln!("{e}");
                    // Heal loop: repair until green or attempts exhausted.
                    // Optional A/B: run base vs heal temperature from the same snapshot,
                    // keep the winner before treating the step as DONE.
                    let mut healed = false;
                    let mut last_err = e;
                    let heal_ab = cfg.heal_ab_enabled();
                    let cool_temp = ollama.configured_heal_temperature();
                    if heal_ab {
                        eprintln!(
                            "[vibe] checkpoint heal A/B enabled (base={} vs cool={:?})",
                            ollama.base_temperature(),
                            cool_temp
                        );
                    }
                    for attempt in 1..=cli.checkpoint_heal_max {
                        let heal_goal = build_heal_goal(cp, &work, &last_err);
                        let baseline = snapshot_origin_sources(&work);
                        let test_snap_heal = collect_test_snapshot(&work);

                        let winner = if heal_ab {
                            let cool = cool_temp.unwrap_or(ollama.base_temperature());
                            println!(
                                "→ checkpoint heal {attempt}/{} A/B for `{cp}`…",
                                cli.checkpoint_heal_max
                            );
                            let branch_a = run_heal_branch(
                                "A",
                                ollama.base_temperature(),
                                &scen,
                                &work,
                                cp,
                                &heal_goal,
                                &agent,
                                &ollama,
                                &fallback,
                                &baseline,
                                &test_snap_heal,
                                &test_snap,
                            )
                            .await;
                            let branch_b = run_heal_branch(
                                "B",
                                cool,
                                &scen,
                                &work,
                                cp,
                                &heal_goal,
                                &agent,
                                &ollama,
                                &fallback,
                                &baseline,
                                &test_snap_heal,
                                &test_snap,
                            )
                            .await;
                            let idx = pick_heal_ab_winner(&branch_a, &branch_b);
                            let win = if idx == 0 { branch_a } else { branch_b };
                            // Re-apply winner onto clean baseline so origin matches the chosen branch.
                            restore_origin_sources(&work, &baseline);
                            ensure_origin_test_snapshot(&work, &test_snap_heal);
                            ensure_origin_test_snapshot(&work, &test_snap);
                            if let Ok(hs) = ShadowWorkspace::new(&work) {
                                for (path, content) in &win.files {
                                    let _ = hs.apply_to_origin(path, content);
                                }
                            }
                            ensure_origin_test_snapshot(&work, &test_snap_heal);
                            ensure_origin_test_snapshot(&work, &test_snap);
                            eprintln!(
                                "→ heal A/B winner={} temperature={} ok={}",
                                win.label, win.temperature, win.ok
                            );
                            win
                        } else {
                            println!(
                                "→ checkpoint heal {attempt}/{} for `{cp}`…",
                                cli.checkpoint_heal_max
                            );
                            let t = ollama
                                .configured_heal_temperature()
                                .unwrap_or(ollama.base_temperature());
                            run_heal_branch(
                                "heal",
                                t,
                                &scen,
                                &work,
                                cp,
                                &heal_goal,
                                &agent,
                                &ollama,
                                &fallback,
                                &baseline,
                                &test_snap_heal,
                                &test_snap,
                            )
                            .await
                        };

                        for (path, _) in &winner.files {
                            if !files.iter().any(|p| p == path) {
                                files.push(path.clone());
                            }
                        }

                        if winner.ok {
                            println!(
                                "→ checkpoint `{cp}`… ok (after heal {attempt}{})",
                                if heal_ab {
                                    format!(" winner={}", winner.label)
                                } else {
                                    String::new()
                                }
                            );
                            checkpoint_ok = Some(true);
                            healed = true;
                            break;
                        }
                        eprintln!(
                            "heal attempt {attempt} still failing{}\n{}",
                            if heal_ab {
                                format!(" (winner={} temp={})", winner.label, winner.temperature)
                            } else {
                                String::new()
                            },
                            winner.err
                        );
                        last_err = if winner.err.is_empty() {
                            "heal branch produced no checkpoint error text".into()
                        } else {
                            winner.err
                        };
                    }
                    if !healed {
                        checkpoint_ok = Some(false);
                        checkpoint_err = Some(last_err);
                        if cli.stop_on_checkpoint_fail {
                            results.push(StepResult {
                                id: step.id,
                                phase: step.phase.clone(),
                                ok: false,
                                seconds: started.elapsed().as_secs_f64(),
                                files,
                                checkpoint: step.checkpoint.clone(),
                                checkpoint_ok,
                                summary: heal.summary.clone().unwrap_or_default(),
                                error: checkpoint_err,
                            });
                            hard_fail = true;
                            break;
                        }
                    }
                }
            }
        }

        // Checkpoint is ground truth when present; heal.success alone can be false
        // after sticky-harness noise even when the checkpoint is green.
        let step_ok = match checkpoint_ok {
            Some(true) => true,
            Some(false) => false,
            None => heal.success,
        };
        println!(
            "{} in {:.1}s — {} file(s)",
            if step_ok { "OK" } else { "PARTIAL" },
            started.elapsed().as_secs_f64(),
            files.len()
        );
        results.push(StepResult {
            id: step.id,
            phase: step.phase.clone(),
            ok: step_ok,
            seconds: started.elapsed().as_secs_f64(),
            files,
            checkpoint: step.checkpoint.clone(),
            checkpoint_ok,
            summary: heal.summary.unwrap_or_default(),
            error: checkpoint_err.or(heal.diagnostics),
        });
    }

    let passed = results.iter().filter(|r| r.ok).count();
    let total = results.len();
    println!("\nSCORE: {passed}/{total} steps ok");
    println!("workspace={}", work.display());

    if let Some(path) = cli.json {
        let payload = json!({
            "scenario": cli.scenario,
            "workspace": work,
            "reason_model": reason,
            "from": cli.from,
            "to": to,
            "passed": passed,
            "total": total,
            "results": results,
        });
        if let Err(e) = fs::write(&path, serde_json::to_string_pretty(&payload).unwrap()) {
            eprintln!("write json: {e}");
        } else {
            println!("wrote {}", path.display());
        }
    }

    if hard_fail || passed < total {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_remote_b2b_prompts_count() {
        let path = scenario_dir("remote_b2b_jobs").join("prompts.jsonl");
        let prompts = load_prompts(&path).expect("load");
        assert_eq!(prompts.len(), 100, "expected ~100 prompts");
        assert!(prompts.first().unwrap().id == 1);
        assert!(prompts.last().unwrap().id == 100);
        let cps: Vec<String> = prompts
            .iter()
            .filter_map(|p| p.checkpoint.clone())
            .collect();
        assert!(cps.iter().any(|c| c == "scaffold"));
        assert!(cps.iter().any(|c| c == "filters"));
        assert!(cps.iter().any(|c| c == "e2e"));
    }

    #[test]
    fn pick_heal_ab_prefers_passing_branch() {
        let fail = HealBranchResult {
            label: "A",
            temperature: 0.2,
            ok: false,
            err: "PASS a\nFAIL b".into(),
            files: vec![],
        };
        let pass = HealBranchResult {
            label: "B",
            temperature: 0.05,
            ok: true,
            err: String::new(),
            files: vec![],
        };
        assert_eq!(pick_heal_ab_winner(&fail, &pass), 1);
        assert_eq!(pick_heal_ab_winner(&pass, &fail), 0);
    }

    #[test]
    fn pick_heal_ab_both_pass_prefers_cooler() {
        let warm = HealBranchResult {
            label: "A",
            temperature: 0.2,
            ok: true,
            err: String::new(),
            files: vec![],
        };
        let cool = HealBranchResult {
            label: "B",
            temperature: 0.05,
            ok: true,
            err: String::new(),
            files: vec![],
        };
        assert_eq!(pick_heal_ab_winner(&warm, &cool), 1);
    }

    #[test]
    fn pick_heal_ab_both_fail_prefers_more_passes() {
        let fewer = HealBranchResult {
            label: "A",
            temperature: 0.2,
            ok: false,
            err: "PASS one\nFAIL two".into(),
            files: vec![],
        };
        let more = HealBranchResult {
            label: "B",
            temperature: 0.05,
            ok: false,
            err: "PASS one\nPASS two\nFAIL three".into(),
            files: vec![],
        };
        assert_eq!(pick_heal_ab_winner(&fewer, &more), 1);
    }

    #[test]
    fn checkpoint_runner_fails_on_empty_workspace() {
        let scen = scenario_dir("remote_b2b_jobs");
        let tmp = tempfile::tempdir().unwrap();
        let err = run_checkpoint(&scen, tmp.path(), "scaffold");
        assert!(err.is_err());
    }
}

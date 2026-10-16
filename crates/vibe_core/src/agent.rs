//! Agentic edit loop: plan → act → critique MVP → repair → stop.
//!
//! When a unittest / cargo-test harness is present, DONE is gated on tests
//! actually passing — critique alone cannot finish early.
//!
//! Designed to avoid endless "integration" thrash: ship a concrete MVP that
//! satisfies the prompt, then DONE. Emits a human-readable activity report.

use crate::config::VibeConfig;
use crate::entrypoint::{cli_help_instruction, detect_python_cli_module, PythonCliModule};
use crate::multifile::{parse_multifile_output, scrub_agent_artifacts, ParsedFile};
use crate::ollama::OllamaClient;
use crate::shadow::{MultiHealResult, ShadowWorkspace};
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub struct AgentLoop<'a> {
    pub cfg: &'a VibeConfig,
    pub ollama: &'a OllamaClient,
    pub reason_model: &'a str,
    /// Optional diagnose-only model; advice is fed into reason_model repairs.
    pub debug_model: Option<&'a str>,
}

#[derive(Debug)]
struct Critique {
    done: bool,
    reason: String,
    gaps: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestHarness {
    None,
    PythonUnittest,
    CargoTest,
}

impl<'a> AgentLoop<'a> {
    pub async fn run(
        &self,
        goal: &str,
        _workspace: &Path,
        shadow: &ShadowWorkspace,
        fallback_path: &str,
        greenfield: bool,
    ) -> Result<MultiHealResult> {
        let max_turns = self.cfg.agent_max_turns.max(1);
        let mut touched: BTreeMap<String, String> = BTreeMap::new();
        let mut last_diag: Option<String> = None;
        let mut last_critique_key = String::new();
        let mut stuck_count = 0u32;
        let mut log: Vec<String> = Vec::new();
        // Snapshot acceptance tests at start so mid-run deletes cannot drop the harness.
        let test_snapshot = collect_test_snapshot(shadow.origin_dir());
        let detected = detect_test_harness(shadow.origin_dir());
        // Sticky: if we started with Python tests (on disk or in snapshot), keep gating DONE.
        let harness = if detected == TestHarness::PythonUnittest || !test_snapshot.is_empty() {
            TestHarness::PythonUnittest
        } else {
            detected
        };
        let tests_required =
            harness != TestHarness::None || !test_snapshot.is_empty() || goal_requires_tests(goal);
        if harness != TestHarness::None {
            eprintln!(
                "[vibe] agent test harness detected: {harness:?} (snapshot={} file(s))",
                test_snapshot.len()
            );
            log.push(format!(
                "Test harness: {harness:?} (DONE gated on pass; {} snapshotted test file(s))",
                test_snapshot.len()
            ));
        } else if tests_required {
            log.push("Tests required by goal — refusing DONE-bias without evidence.".into());
        }

        let goal = sanitize_goal(goal);
        log.push(format!("Goal: {}", truncate(&goal, 200)));

        let checkpoint_heal = goal_is_checkpoint_heal(&goal);
        let _heal_temp_guard = self.ollama.heal_mode_guard(checkpoint_heal);
        let small_agent = self.cfg.is_small_agent();
        if small_agent {
            eprintln!("[vibe] agent profile: small (stricter CLI/heal gates)");
            log.push("Agent profile: small".into());
        }
        if checkpoint_heal {
            let heal_t = self
                .cfg
                .effective_heal_temperature()
                .unwrap_or(self.cfg.ollama_temperature);
            log.push(format!(
                "Checkpoint-heal mode: DONE requires CLI -h, not unittest alone (temperature={heal_t})."
            ));
            eprintln!("[vibe] agent heal temperature={heal_t}");
        }
        // With a real test harness, skip the LLM planner — it often invents README/tests
        // and burns a turn. Use a fixed acceptance checklist instead.
        // Checkpoint heal / check failures: diagnose first, then a concrete fix plan.
        let mut plan = if checkpoint_heal {
            eprintln!("[vibe] agent diagnose + fix plan (heal)…");
            log.push("Diagnosing failure and drafting fix plan…".into());
            let cli0 = detect_python_cli_module(shadow.shadow_dir(), &touched, &goal);
            let plan = self
                .diagnose_and_plan_fix(&goal, shadow.shadow_dir(), cli0.as_ref(), None)
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "heal diagnose failed; falling back to MVP plan");
                    format!("Fix the failing check described in the goal.\n(diagnose error: {e})")
                });
            let plan_preview = plan.lines().take(14).collect::<Vec<_>>().join("\n");
            eprintln!("[vibe] agent fix plan:\n{plan_preview}");
            log.push(format!("Fix plan:\n{plan_preview}"));
            plan
        } else if harness != TestHarness::None {
            let plan = harness_plan(harness, fallback_path);
            eprintln!("[vibe] agent plan (harness, fixed):\n{plan}");
            log.push(format!("Plan (fixed for harness):\n{plan}"));
            plan
        } else {
            eprintln!("[vibe] agent plan…");
            log.push("Planning MVP checklist…".into());
            let plan = self.plan_mvp(&goal, greenfield, harness).await?;
            let plan_preview = plan.lines().take(10).collect::<Vec<_>>().join("\n");
            eprintln!("[vibe] agent plan:\n{plan_preview}");
            log.push(format!("Plan:\n{plan_preview}"));
            plan
        };
        let mut diagnosed_check_failure = checkpoint_heal;

        let mut finish_reason = String::from("turn budget");
        let mut last_test_fail: Option<String> = None;
        // Full unittest output (for probing the failing call; distill can drop it).
        let mut last_test_raw: Option<String> = None;
        let mut same_class_streak: u32 = 0;
        let mut last_repair_class: Option<FailureClass> = None;
        let required_exports = extract_required_python_exports(shadow.origin_dir());
        let origin_stub = read_origin_stub(shadow.origin_dir(), fallback_path);
        if !required_exports.is_empty() {
            log.push(format!("Required exports: {}", required_exports.join(", ")));
            eprintln!(
                "[vibe] agent required exports: {}",
                required_exports.join(", ")
            );
        }

        for turn in 1..=max_turns {
            info!(turn, max_turns, "agent turn");
            log.push(format!("— Turn {turn}/{max_turns} —"));

            // Keep scoring tests intact in the shadow tree (from start-of-run snapshot).
            restore_protected_tests(shadow, &test_snapshot);

            // Recompute each turn: package / CLI may appear after the first act.
            let cli_mod = detect_python_cli_module(shadow.shadow_dir(), &touched, &goal);
            // Hard CLI gate only during checkpoint heal — normal steps use unittest alone.
            let cli_gate = checkpoint_heal;

            // Run harness against current shadow before critique / finish.
            let mut tests_ok = if harness != TestHarness::None && !touched.is_empty() {
                match run_workspace_tests(shadow.shadow_dir(), harness) {
                    Ok(()) => {
                        eprintln!("[vibe] agent turn {turn}: tests PASSED");
                        log.push("Tests passed.".into());
                        last_test_fail = None;
                        last_test_raw = None;
                        true
                    }
                    Err(out) => {
                        let brief = distill_test_failure(&out, 1200);
                        eprintln!(
                            "[vibe] agent turn {turn}: tests FAILED — {}",
                            brief.chars().take(160).collect::<String>()
                        );
                        log.push(format!("Tests failed:\n{brief}"));
                        last_test_raw = Some(out);
                        last_test_fail = Some(brief);
                        false
                    }
                }
            } else {
                true // no harness or nothing written yet
            };

            // Checkpoint heal: unittest green is not enough — `python -m pkg -h` must exit 0.
            // Exact help tokens stay in scenario checkpoint scripts, not this gate.
            let mut cli_ok = true;
            if tests_ok && cli_gate && !touched.is_empty() {
                if let Some(cli) = &cli_mod {
                    match verify_python_cli(shadow.shadow_dir(), cli, &goal, false) {
                        Ok(()) => {
                            eprintln!(
                                "[vibe] agent turn {turn}: CLI -h PASSED ({})",
                                cli.help_display()
                            );
                            log.push(format!("CLI -h passed ({})", cli.help_display()));
                            cli_ok = true;
                        }
                        Err(err) => {
                            eprintln!(
                                "[vibe] agent turn {turn}: CLI -h FAILED — {}",
                                err.chars().take(160).collect::<String>()
                            );
                            log.push(format!("CLI -h failed:\n{}", truncate(&err, 800)));
                            let help_cmd = cli.help_display();
                            last_test_fail = Some(format!(
                                "CLI entrypoint gate failed (unittest alone is NOT enough).\n\
Fix so `{help_cmd}` exits 0.\n\
- Fix ModuleNotFoundError by creating the missing module OR removing the bad import.\n\
- Lazy-import optional heavy deps inside functions — not at module import time for -h.\n\
- Declare third-party deps in pyproject.toml / requirements.txt.\n\
- Do not leave stub/empty modules that checkpoints require.\n\n{err}"
                            ));
                            cli_ok = false;
                            tests_ok = false; // block DONE / early finish
                        }
                    }
                } else {
                    // Heal without a detected CLI yet: still refuse unittest-only DONE.
                    cli_ok = true;
                }
            }

            if tests_ok {
                same_class_streak = 0;
                last_repair_class = None;
                last_test_raw = None;
            } else if !diagnosed_check_failure {
                // First red check this run: diagnose issues and replace the plan (plan-only).
                if let Some(fail) = last_test_fail.clone() {
                    eprintln!("[vibe] agent diagnose + fix plan (failed check)…");
                    log.push("Diagnosing failed check and drafting fix plan…".into());
                    match self
                        .diagnose_and_plan_fix(
                            &goal,
                            shadow.shadow_dir(),
                            cli_mod.as_ref(),
                            Some(&fail),
                        )
                        .await
                    {
                        Ok(p) => {
                            let preview = p.lines().take(14).collect::<Vec<_>>().join("\n");
                            eprintln!("[vibe] agent fix plan:\n{preview}");
                            log.push(format!("Fix plan:\n{preview}"));
                            plan = p;
                            diagnosed_check_failure = true;
                        }
                        Err(e) => {
                            warn!(error = %e, "check-failure diagnose failed");
                            log.push(format!("Diagnose failed: {e:#}"));
                        }
                    }
                }
            }

            let observation = observe_agent_files(
                &touched,
                greenfield,
                fallback_path,
                &origin_stub,
                &required_exports,
            );

            let mut critique = if touched.is_empty() {
                Critique {
                    done: false,
                    reason: "Nothing written yet — implement the MVP plan now.".into(),
                    gaps: format!("Implement this MVP checklist:\n{plan}"),
                }
            } else if let Some(fail) = &last_test_fail {
                let cli_fail = fail_looks_like_cli_gate(fail);
                Critique {
                    done: false,
                    reason: if cli_fail {
                        "CLI -h / entrypoint check is failing — follow the fix plan (create missing files)."
                            .into()
                    } else {
                        "Unit tests are failing — fix the implementation.".into()
                    },
                    gaps: if cli_fail {
                        format!(
                            "Apply the FIX PLAN paths exactly (e.g. create `__main__.py` if named).\n\
Do NOT only rewrite cli.py when the failure is a missing package entrypoint.\n\
Do NOT edit test_*.py.\n\n### Failure:\n{fail}\n\n### Fix plan:\n{plan}"
                        )
                    } else {
                        format!(
                            "Make the existing tests pass. Do NOT edit test_*.py / test files.\n\
Fix solution code only.\n\n### Test output:\n{fail}"
                        )
                    },
                }
            } else {
                eprintln!("[vibe] agent turn {turn}/{max_turns}: critique");
                log.push("Critiquing agent-written files vs MVP…".into());
                self.critique(
                    &goal,
                    &plan,
                    &observation,
                    turn,
                    max_turns,
                    harness,
                    tests_required,
                    checkpoint_heal || cli_gate,
                    cli_mod.as_ref(),
                )
                .await?
            };

            // Never trust LLM DONE while tests/CLI gates are red.
            if !tests_ok {
                critique.done = false;
            }
            if checkpoint_heal && !cli_ok {
                critique.done = false;
            }

            let critique_line = format!(
                "Critique: {} — {}",
                if critique.done { "DONE" } else { "CONTINUE" },
                truncate(&critique.reason, 160)
            );
            eprintln!(
                "[vibe] agent turn {turn}: critique done={} — {}",
                critique.done,
                critique.reason.chars().take(140).collect::<String>()
            );
            log.push(critique_line);

            let key = critique_fingerprint(&critique);
            if !touched.is_empty() && key == last_critique_key {
                stuck_count += 1;
            } else {
                stuck_count = 0;
            }
            last_critique_key = key;

            // With a harness / required tests: only finish early when tests are green.
            // Without: keep previous MVP / anti-thrash shortcuts.
            let force_finish = if tests_required {
                (tests_ok && critique.done && !touched.is_empty())
                    || (tests_ok && turn == max_turns && !touched.is_empty())
                    || (turn == max_turns && !touched.is_empty())
            } else {
                (!touched.is_empty() && stuck_count >= 1)
                    || (turn == max_turns && !touched.is_empty())
                    || (critique.done && !touched.is_empty())
                    || (looks_like_mvp(&touched) && turn >= 2 && !touched.is_empty())
            };

            if force_finish {
                let files = touched_to_files(&touched);
                let mut errors = Vec::new();
                for f in &files {
                    if is_protected_test_path(&f.path) {
                        continue;
                    }
                    if let Err(e) = shadow.verify_single_file(&f.path) {
                        errors.push(format!("{}: {e}", f.path));
                    }
                }
                if errors.is_empty() {
                    finish_reason = if tests_required && tests_ok {
                        "tests passed".into()
                    } else if tests_required && !tests_ok {
                        "turn budget with tests still failing".into()
                    } else if critique.done {
                        "critique said DONE".into()
                    } else if stuck_count >= 1 {
                        "stopped repeating the same critique (anti-thrash)".into()
                    } else if looks_like_mvp(&touched) {
                        "MVP heuristic (runnable entrypoint present)".into()
                    } else {
                        "turn budget reached".into()
                    };
                    eprintln!(
                        "[vibe] agent finishing ({finish_reason}) with {} file(s)",
                        files.len()
                    );
                    log.push(format!("Finished: {finish_reason}"));
                    log.push(format_files_written(&touched));
                    let success = !touched.is_empty() && (!tests_required || tests_ok);
                    let summary = self
                        .narrate(&goal, &plan, &log, &touched, success)
                        .await
                        .unwrap_or_else(|_| {
                            fallback_summary(&goal, &touched, &finish_reason, success)
                        });
                    return Ok(MultiHealResult {
                        files,
                        attempts: turn,
                        diagnostics: last_test_fail.clone().or(last_diag),
                        success,
                        summary: Some(summary),
                        log,
                    });
                }
                last_diag = Some(errors.join("\n"));
                log.push(format!(
                    "Syntax errors before finish: {}",
                    truncate(&errors.join("; "), 200)
                ));
                eprintln!("[vibe] agent turn {turn}: syntax repair before finish");
                self.repair_syntax(
                    shadow,
                    &goal,
                    &mut touched,
                    &errors,
                    &mut log,
                    &test_snapshot,
                )
                .await?;
                continue;
            }

            if !critique.done && !touched.is_empty() {
                log.push(format!("Gaps to close:\n{}", truncate(&critique.gaps, 400)));
            }

            eprintln!("[vibe] agent turn {turn}/{max_turns}: act");
            log.push("Generating / updating files…".into());
            let act_prompt = if let (Some(fail), true) =
                (last_test_fail.as_deref(), harness != TestHarness::None)
            {
                let class = classify_test_failure(fail);
                if last_repair_class == Some(class) {
                    same_class_streak = same_class_streak.saturating_add(1);
                } else {
                    same_class_streak = 1;
                    last_repair_class = Some(class);
                }
                // After 2 failed repairs of the same class, force a from-scratch rewrite.
                let force_rewrite = same_class_streak >= 3;
                let probe_src = last_test_raw.as_deref().unwrap_or(fail);
                let probe = if harness == TestHarness::PythonUnittest {
                    probe_failing_python_case(shadow.shadow_dir(), probe_src)
                } else {
                    None
                };
                if let Some(ref p) = probe {
                    log.push(format!("Failing-case probe:\n{}", truncate(p, 300)));
                    eprintln!(
                        "[vibe] agent probe: {}",
                        p.chars().take(120).collect::<String>()
                    );
                }
                let static_hints = if harness == TestHarness::PythonUnittest {
                    build_static_fix_hints(
                        shadow.shadow_dir(),
                        fallback_path,
                        class,
                        fail,
                        probe.as_deref(),
                    )
                } else {
                    None
                };
                if let Some(ref h) = static_hints {
                    log.push(format!("Static diagnosis:\n{}", truncate(h, 400)));
                    eprintln!(
                        "[vibe] agent static hints: {}",
                        h.chars().take(140).collect::<String>()
                    );
                }
                let debug_advice = if let Some(dm) = self.debug_model {
                    let dbg_prompt = build_debug_diagnose_prompt(
                        &goal,
                        fail,
                        class,
                        probe.as_deref(),
                        static_hints.as_deref(),
                        &observation,
                        force_rewrite,
                    );
                    eprintln!("[vibe] agent debug diagnose model={dm}");
                    match self.generate_llm(dm, &dbg_prompt, Some(1024)).await {
                        Ok(raw) => {
                            let advice = scrub_reasoning_output(&raw);
                            if advice.trim().is_empty() {
                                None
                            } else {
                                let advice = truncate(&advice, 1600);
                                log.push(format!(
                                    "Debug model advice:\n{}",
                                    truncate(&advice, 400)
                                ));
                                eprintln!(
                                    "[vibe] agent debug advice: {}",
                                    advice.chars().take(140).collect::<String>()
                                );
                                Some(advice)
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "debug_model diagnose failed; continuing without it");
                            log.push(format!("Debug model failed: {e}"));
                            None
                        }
                    }
                } else {
                    None
                };
                if force_rewrite {
                    eprintln!(
                        "[vibe] agent repair: rewrite-from-scratch (streak={same_class_streak} class={class:?})"
                    );
                    log.push(format!(
                        "Rewrite-from-scratch (same failure class {class:?} × {same_class_streak})"
                    ));
                }
                let obs_for_repair = if force_rewrite {
                    // Hide the broken body so the model does not patch a bad algorithm.
                    observe_agent_files(
                        &BTreeMap::new(),
                        greenfield,
                        fallback_path,
                        &origin_stub,
                        &required_exports,
                    )
                } else {
                    observation.clone()
                };
                build_repair_prompt(
                    &goal,
                    &obs_for_repair,
                    fail,
                    fallback_path,
                    &required_exports,
                    force_rewrite,
                    probe.as_deref(),
                    static_hints.as_deref(),
                    debug_advice.as_deref(),
                )
            } else {
                build_act_prompt(
                    &goal,
                    &plan,
                    &observation,
                    &critique,
                    greenfield,
                    turn,
                    harness,
                    &required_exports,
                    small_agent || checkpoint_heal,
                    cli_mod.as_ref(),
                )
            };
            let raw = match self
                .generate_llm(self.reason_model, &act_prompt, Some(8192))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    // Do not abort the whole agent run — keep prior staged files and retry next turn.
                    warn!(turn, error = %e, "agent act generate failed after retries");
                    eprintln!("[vibe] agent turn {turn}: generate failed after retries — {e:#}");
                    log.push(format!("Act generate failed (will retry next turn): {e:#}"));
                    last_diag = Some(format!("ollama generate failed on turn {turn}: {e:#}"));
                    continue;
                }
            };

            let mut files = parse_multifile_output(&raw, fallback_path);
            let before = files.len();
            files.retain(|f| !is_protected_test_path(&f.path));
            if files.len() < before {
                log.push("Ignored agent edits to protected test files.".into());
                eprintln!("[vibe] agent turn {turn}: dropped protected test file edits");
            }
            if files.is_empty() {
                warn!(turn, "agent act produced no files");
                last_diag = Some("agent produced no parseable @@@ FILE blocks".into());
                log.push("Act produced no parseable file blocks — retrying.".into());
                continue;
            }

            let written: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
            let snapshot = touched.clone();
            let staged = stage_and_record(shadow, &files, &mut touched)?;
            restore_protected_tests(shadow, &test_snapshot);

            // Soft gate: only discard empty scrub / missing required exports.
            // Syntax errors are repaired in-place (hard discard previously wiped
            // touched and crashed Easy from ~81% → ~44%).
            if staged.is_empty() {
                let reason = "no file content left after scrub (do not put @@@ / markdown / prose inside the file body)";
                eprintln!("[vibe] agent turn {turn}: discarding empty write");
                log.push(format!("Discarded empty write: {reason}"));
                revert_shadow_writes(shadow, &snapshot, &written, &test_snapshot)?;
                touched.clear();
                touched.extend(snapshot);
                last_diag = Some(reason.into());
                last_test_fail = Some(format!(
                    "Rejected empty write. Re-emit `{fallback_path}` with real code only.\n\
Keep exports: {}\n\nStub:\n{}",
                    required_exports.join(", "),
                    truncate(&origin_stub, 1200)
                ));
                continue;
            }

            let mut export_reject: Option<String> = None;
            if harness == TestHarness::PythonUnittest && !required_exports.is_empty() {
                let solution = touched
                    .get(fallback_path)
                    .or_else(|| touched.get("solution.py"));
                match solution {
                    Some(body) => {
                        if let Err(e) = validate_python_exports(body, &required_exports) {
                            export_reject = Some(e);
                        }
                    }
                    None => {
                        export_reject = Some(format!(
                            "did not write `{fallback_path}` with required exports: {}",
                            required_exports.join(", ")
                        ));
                    }
                }
            }

            if let Some(reason) = export_reject {
                eprintln!(
                    "[vibe] agent turn {turn}: discarding wrong-API write — {}",
                    reason.chars().take(160).collect::<String>()
                );
                log.push(format!(
                    "Discarded wrong-API write:\n{}",
                    truncate(&reason, 300)
                ));
                revert_shadow_writes(shadow, &snapshot, &written, &test_snapshot)?;
                touched.clear();
                touched.extend(snapshot);
                last_diag = Some(reason.clone());
                last_test_fail = Some(format!(
                    "Rejected write — missing required API.\n{reason}\n\n\
Re-emit `{fallback_path}` keeping exports: {}\n\nStub:\n{}",
                    required_exports.join(", "),
                    truncate(&origin_stub, 1200)
                ));
                continue;
            }

            if let Some(reason) = literal_sprawl_in_writes(&touched, &written) {
                eprintln!(
                    "[vibe] agent turn {turn}: discarding literal sprawl — {}",
                    reason.chars().take(160).collect::<String>()
                );
                log.push(format!(
                    "Discarded literal sprawl:\n{}",
                    truncate(&reason, 300)
                ));
                revert_shadow_writes(shadow, &snapshot, &written, &test_snapshot)?;
                touched.clear();
                touched.extend(snapshot);
                last_diag = Some(reason.clone());
                last_test_fail = Some(format!(
                    "Rejected write — literal sprawl / enumerate-to-impress.\n{reason}\n\n\
Shrink hardcoded tables to ≤15 real entries (or use a tiny config/pattern). \
No filler or fake domains/TLDs. Re-emit the file(s) lean."
                ));
                continue;
            }

            log.push(format!("Wrote/updated: {}", written.join(", ")));
            if let Some(hint) = duplicate_module_hint(shadow.shadow_dir(), &written) {
                eprintln!("[vibe] agent turn {turn}: sprawl hint — {hint}");
                log.push(format!("Sprawl hint: {hint}"));
            }
            let mut errors = Vec::new();
            for path in &staged {
                if let Err(e) = shadow.verify_single_file(path) {
                    errors.push(format!("{path}: {e}"));
                }
            }
            if !errors.is_empty() {
                last_diag = Some(errors.join("\n"));
                log.push(format!(
                    "Syntax check failed: {}",
                    truncate(&errors.join("; "), 200)
                ));
                eprintln!("[vibe] agent turn {turn}: syntax repair");
                self.repair_syntax(
                    shadow,
                    &goal,
                    &mut touched,
                    &errors,
                    &mut log,
                    &test_snapshot,
                )
                .await?;
                // Re-validate exports after repair (repair can rename/drop APIs).
                if harness == TestHarness::PythonUnittest && !required_exports.is_empty() {
                    let solution = touched
                        .get(fallback_path)
                        .or_else(|| touched.get("solution.py"));
                    if let Some(body) = solution {
                        if let Err(e) = validate_python_exports(body, &required_exports) {
                            eprintln!("[vibe] agent turn {turn}: repair broke exports — reverting");
                            log.push(format!("Repair broke exports: {e}"));
                            revert_shadow_writes(shadow, &snapshot, &written, &test_snapshot)?;
                            touched.clear();
                            touched.extend(snapshot);
                            last_test_fail = Some(format!(
                                "Repair dropped required exports: {e}\nKeep: {}\nStub:\n{}",
                                required_exports.join(", "),
                                truncate(&origin_stub, 1200)
                            ));
                            continue;
                        }
                    }
                }
            } else {
                last_diag = None;
                log.push("Syntax check passed.".into());
            }
            eprintln!("[vibe] agent turn {turn}: staged {}", written.join(", "));

            // Recompute after act — package / CLI may have just been created this turn.
            let cli_mod_after = detect_python_cli_module(shadow.shadow_dir(), &touched, &goal);
            let cli_gate_after = checkpoint_heal;

            // Fast-path: if harness is green right after this act, stop — no extra critique turn.
            // Skip for checkpoint-heal (must clear -h too).
            if harness != TestHarness::None && !cli_gate_after {
                restore_protected_tests(shadow, &test_snapshot);
                match run_workspace_tests(shadow.shadow_dir(), harness) {
                    Ok(()) => {
                        eprintln!("[vibe] agent turn {turn}: tests PASSED after act — finishing");
                        log.push("Tests passed after act.".into());
                        finish_reason = "tests passed after act".into();
                        log.push(format!("Finished: {finish_reason}"));
                        log.push(format_files_written(&touched));
                        let summary = self
                            .narrate(&goal, &plan, &log, &touched, true)
                            .await
                            .unwrap_or_else(|_| {
                                fallback_summary(&goal, &touched, &finish_reason, true)
                            });
                        return Ok(MultiHealResult {
                            files: touched_to_files(&touched),
                            attempts: turn,
                            diagnostics: None,
                            success: true,
                            summary: Some(summary),
                            log,
                        });
                    }
                    Err(out) => {
                        let brief = distill_test_failure(&out, 1200);
                        last_test_raw = Some(out);
                        last_test_fail = Some(brief.clone());
                        log.push(format!(
                            "Tests still failing after act:\n{}",
                            truncate(&brief, 400)
                        ));
                    }
                }
            } else if cli_gate_after {
                // Checkpoint heal: require tests + runnable `python -m pkg -h` (no token scrape).
                restore_protected_tests(shadow, &test_snapshot);
                if harness != TestHarness::None {
                    if let Err(out) = run_workspace_tests(shadow.shadow_dir(), harness) {
                        let brief = distill_test_failure(&out, 1200);
                        last_test_raw = Some(out);
                        last_test_fail = Some(brief);
                    }
                }
                if last_test_fail.is_none() {
                    if let Some(cli) = &cli_mod_after {
                        if let Err(err) = verify_python_cli(shadow.shadow_dir(), cli, &goal, false)
                        {
                            let help_cmd = cli.help_display();
                            last_test_fail = Some(format!(
                                "CLI entrypoint gate failed after act.\n\
Fix `{help_cmd}` (lazy-import optional deps; create missing modules).\n\n{err}"
                            ));
                            log.push(format!(
                                "CLI -h still failing after act:\n{}",
                                truncate(&err, 400)
                            ));
                            eprintln!(
                                "[vibe] agent turn {turn}: CLI -h FAILED after act — continuing"
                            );
                        } else {
                            eprintln!(
                                "[vibe] agent turn {turn}: tests+CLI PASSED after act ({})",
                                cli.help_display()
                            );
                            log.push(format!("CLI -h passed after act ({})", cli.help_display()));
                        }
                    }
                }
            }
        }

        log.push(format!(
            "Stopped after {max_turns} turns ({finish_reason})."
        ));
        log.push(format_files_written(&touched));
        let success = !touched.is_empty() && last_test_fail.is_none();
        let summary = self
            .narrate(&goal, &plan, &log, &touched, success)
            .await
            .unwrap_or_else(|_| fallback_summary(&goal, &touched, &finish_reason, success));

        Ok(MultiHealResult {
            files: touched_to_files(&touched),
            attempts: max_turns,
            diagnostics: last_test_fail.or_else(|| {
                Some(last_diag.unwrap_or_else(|| format!("agent stopped after {max_turns} turns")))
            }),
            success,
            summary: Some(summary),
            log,
        })
    }

    async fn narrate(
        &self,
        goal: &str,
        plan: &str,
        log: &[String],
        touched: &BTreeMap<String, String>,
        success: bool,
    ) -> Result<String> {
        let files = touched
            .keys()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        let steps = log
            .iter()
            .rev()
            .take(20)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            r#"Write a short status update (4–7 sentences, plain text) for the user about what the coding agent just did.
Be concrete: mention files created/updated and how to run the result if obvious.
Do not use markdown headings. Do not invent files not listed.

Outcome: {}
User request: {}
MVP plan (abbrev): {}
Files:
{}
Activity log:
{}
"#,
            if success {
                "completed"
            } else {
                "partial / stopped"
            },
            truncate(goal, 300),
            truncate(plan, 400),
            if files.is_empty() {
                "(none)".into()
            } else {
                files
            },
            truncate(&steps, 1500)
        );
        let raw = self
            .generate_llm(self.reason_model, &prompt, Some(450))
            .await
            .context("agent narrate")?;
        Ok(raw.trim().to_string())
    }

    async fn generate_llm(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: Option<i32>,
    ) -> Result<String> {
        self.ollama
            .generate_with_retries(model, prompt, max_tokens, self.cfg.ollama_generate_retries)
            .await
    }

    async fn plan_mvp(&self, goal: &str, greenfield: bool, harness: TestHarness) -> Result<String> {
        let mode = if greenfield {
            "greenfield (new files only)"
        } else {
            "edit existing project"
        };
        let test_rule = match harness {
            TestHarness::None => String::new(),
            TestHarness::PythonUnittest => {
                "\n- Existing `test_*.py` files are the acceptance criteria — make them pass; do not rewrite tests."
                    .into()
            }
            TestHarness::CargoTest => {
                "\n- `cargo test` must pass; do not gut or rewrite existing tests.".into()
            }
        };
        let prompt = format!(
            r#"You are a pragmatic tech lead. Turn the user request into a MINIMAL acceptance checklist.

Mode: {mode}

### User request:
{goal}

Reply with ONLY a short checklist (5–8 bullets). Each bullet must name a concrete deliverable
(file or runnable behavior), e.g.:
- scripts/job_search/cli.py — argparse CLI with query + skills flags
- scripts/job_search/search.py — fetch/search helper with requests
- scripts/requirements.txt — deps
- README with how to run

Rules:
- Prefer ONE small package that runs end-to-end.
- No enterprise architecture, no abstract Integration classes, no frameworks unless asked.
- Web search / job search means a working script that queries public job APIs or scrapes listings — not UML.
- Stop at MVP. Do not list polish items.{test_rule}
"#
        );
        self.generate_llm(self.reason_model, &prompt, Some(700))
            .await
            .context("agent plan")
    }

    /// Diagnose a failed check (checkpoint / CLI / unittest) and produce a concrete fix plan.
    async fn diagnose_and_plan_fix(
        &self,
        goal: &str,
        root: &Path,
        cli_mod: Option<&PythonCliModule>,
        extra_fail: Option<&str>,
    ) -> Result<String> {
        let help_blob = match cli_mod {
            Some(cli) => match run_python_module_help(root, &cli.module) {
                Ok(h) => format!(
                    "### Current `{}` output\n{}",
                    cli.help_display(),
                    truncate(&h, 1500)
                ),
                Err(e) => format!("### Current CLI help\n(failed: {e})"),
            },
            None => "### Current CLI help\n(no Python CLI module detected yet)".into(),
        };
        let files_blob = heal_context_files(root, cli_mod);
        let fail_blob = extra_fail
            .map(|f| format!("### Latest check / test failure\n{}\n", truncate(f, 1800)))
            .unwrap_or_default();
        let prompt = format!(
            r#"You are diagnosing a FAILED automated check on a coding-agent workspace.
Do NOT write code yet. Diagnose, then produce a minimal fix plan.

### Goal / failure context
{goal}

{fail_blob}
{help_blob}

### Relevant file snippets
{files_blob}

Reply in EXACTLY this format:

@@@ DIAGNOSIS
- bullet: what is wrong (quote missing help tokens, import errors, missing flags, etc.)
- bullet: …

@@@ FIX PLAN
- path: `relative/path.py`
  ```python
  # complete file contents when creating/replacing a small file
  ```
- Prefer exact CLI names from the failure (help_contains 'X' ⇒ X must appear verbatim in -h)
- For `No module named pkg.__main__` / package cannot be executed: path MUST be `pkg/__main__.py` (not cli.py)
- Do not rename working APIs; do not delete tests
- Keep to 2–5 path bullets; minimal fix only
"#
        );
        let raw = self
            .generate_llm(self.reason_model, &prompt, Some(900))
            .await
            .context("agent heal diagnose")?;
        Ok(normalize_heal_plan(&raw))
    }

    async fn critique(
        &self,
        goal: &str,
        plan: &str,
        observation: &str,
        turn: u32,
        max_turns: u32,
        harness: TestHarness,
        tests_required: bool,
        cli_gate: bool,
        cli_mod: Option<&PythonCliModule>,
    ) -> Result<Critique> {
        let help_hint = cli_help_instruction(cli_mod);
        let test_bias = if cli_gate {
            format!(
                "DONE is STRICT.\nUnit tests green is NOT enough.\nDONE only if {help_hint} AND the checkpoint/request failure text is addressed.\nCONTINUE on any ModuleNotFoundError, broken imports, missing modules, or stub files.\nFix the traceback — do not thrash the same import back and forth."
            )
        } else if tests_required || harness != TestHarness::None {
            "Tests are REQUIRED for DONE.\nTests already passed in this turn — verify the implementation matches the request.\nDONE only if the solution looks complete AND tests are green.\nCONTINUE if code is stubbed/empty, wrong, or tests were deleted."
                .to_string()
        } else {
            "Be BIASED toward DONE.\nDONE when a user can run the main entrypoint OR checklist items are mostly present.\nCONTINUE only for HARD blockers.\nIf unsure, choose DONE."
                .to_string()
        };
        let prompt = format!(
            r#"You review agent-written files against an MVP checklist.

### User request:
{goal}

### MVP checklist (acceptance criteria):
{plan}

### Agent-written files ONLY (ignore any other repo files):
{observation}

Turn {turn}/{max_turns}.

{test_bias}

FORBIDDEN gaps (do not invent these):
- "better integration", "connect components", "add architecture", "more abstraction"
- Rewriting working code for style
- Features not in the checklist
- Editing or replacing test files

Reply EXACTLY:
@@@ STATUS: DONE
or
@@@ STATUS: CONTINUE
@@@ REASON: one sentence
@@@ GAPS:
- path/to/file.py — specific missing behavior
"#
        );
        let raw = self
            .generate_llm(self.reason_model, &prompt, Some(600))
            .await
            .context("agent critique")?;
        Ok(parse_critique(&raw))
    }

    async fn repair_syntax(
        &self,
        shadow: &ShadowWorkspace,
        goal: &str,
        touched: &mut BTreeMap<String, String>,
        errors: &[String],
        log: &mut Vec<String>,
        test_snapshot: &BTreeMap<String, String>,
    ) -> Result<()> {
        log.push("Repairing syntax…".into());
        let bundle = touched
            .iter()
            .filter(|(p, _)| !is_protected_test_path(p))
            .map(|(p, c)| format!("@@@ FILE: {p}\n{c}"))
            .collect::<Vec<_>>()
            .join("\n");
        let repair = format!(
            "Fix ONLY syntax errors. Re-emit every file with @@@ FILE markers. Do not redesign. Do not edit test files.\n\n### Goal:\n{goal}\n\n### Errors:\n{}\n\n### Files:\n{bundle}",
            errors.join("\n")
        );
        let raw = self
            .generate_llm(self.reason_model, &repair, Some(8192))
            .await?;
        let mut fixed = parse_multifile_output(&raw, "main.py");
        fixed.retain(|f| !is_protected_test_path(&f.path));
        if !fixed.is_empty() {
            let paths: Vec<_> = fixed.iter().map(|f| f.path.clone()).collect();
            stage_and_record(shadow, &fixed, touched)?;
            restore_protected_tests(shadow, test_snapshot);
            log.push(format!("Repaired: {}", paths.join(", ")));
        } else {
            log.push("Repair produced no parseable files.".into());
        }
        Ok(())
    }
}

fn detect_test_harness(origin: &Path) -> TestHarness {
    if has_python_tests(origin) {
        return TestHarness::PythonUnittest;
    }
    if origin.join("Cargo.toml").exists() {
        // Prefer cargo test for rust crates (hand fixtures, etc.).
        return TestHarness::CargoTest;
    }
    TestHarness::None
}

fn has_python_tests(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("test_") && name.ends_with(".py") {
            return true;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let n = name.as_ref();
            if matches!(n, ".vibe" | ".git" | "target" | "node_modules" | ".venv") {
                continue;
            }
            if has_python_tests(&entry.path()) {
                return true;
            }
        }
    }
    false
}

fn is_protected_test_path(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    (name.starts_with("test_") && name.ends_with(".py")) || name == "test_solution.py"
}

/// Hard anti-sprawl: detect enumerate-to-impress literal tables (e.g. huge host dicts).
///
/// Thresholds intentionally allow a normal ~10–15 entry allowlist.
fn literal_sprawl_smell(path: &str, body: &str) -> Option<String> {
    static DICT_ENTRY_RE: OnceLock<Regex> = OnceLock::new();
    static DOMAIN_RE: OnceLock<Regex> = OnceLock::new();
    let dict_re = DICT_ENTRY_RE
        .get_or_init(|| Regex::new(r#"(?m)^\s*['"][^'"]+['"]\s*:"#).expect("dict entry re"));
    let domain_re = DOMAIN_RE.get_or_init(|| {
        Regex::new(r#"['"]([a-zA-Z0-9.-]+\.(?:com|org|net|io|co|dev|eu|uk|info))['"]"#)
            .expect("domain re")
    });

    let dict_entries = dict_re.find_iter(body).count();
    if dict_entries >= 40 {
        return Some(format!(
            "{path}: {dict_entries} string-keyed dict entries (≥40) — shrink to ≤15 real hosts/keys or use config"
        ));
    }

    let domains: Vec<String> = domain_re
        .captures_iter(body)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_ascii_lowercase()))
        .collect();
    if domains.len() >= 20 {
        return Some(format!(
            "{path}: {} domain-like string literals (≥20) — keep a small real allowlist (≤15)",
            domains.len()
        ));
    }

    let mut prefix_counts: BTreeMap<String, usize> = BTreeMap::new();
    for d in &domains {
        let label = d.split('.').next().unwrap_or(d);
        // Strip trailing digits so remotive1/remotive2 still group if ever emitted.
        let prefix: String = label
            .chars()
            .rev()
            .skip_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .take(12)
            .collect();
        if prefix.len() >= 4 {
            *prefix_counts.entry(prefix).or_default() += 1;
        }
    }
    if let Some((pref, n)) = prefix_counts.into_iter().max_by_key(|(_, n)| *n) {
        if n >= 8 {
            return Some(format!(
                "{path}: {n} domains share prefix `{pref}` (≥8) — drop filler/fake TLD variants"
            ));
        }
    }
    None
}

fn literal_sprawl_in_writes(
    touched: &BTreeMap<String, String>,
    written: &[String],
) -> Option<String> {
    for path in written {
        if let Some(body) = touched.get(path) {
            if let Some(reason) = literal_sprawl_smell(path, body) {
                return Some(reason);
            }
        }
    }
    None
}

/// Soft anti-sprawl: warn when a new file stem collides with an existing sibling.
fn duplicate_module_hint(root: &Path, written: &[String]) -> Option<String> {
    let mut hints = Vec::new();
    for path in written {
        let p = Path::new(path);
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(parent) = p.parent() else {
            continue;
        };
        // Any package-shaped path or scripts/ — not scenario-specific.
        let under_package = p
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .map(|s| !s.starts_with('.') && s != "tests" && s != "test")
            .unwrap_or(false);
        if !under_package && !path.starts_with("scripts/") {
            continue;
        }
        let dir = root.join(parent);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".py") {
                continue;
            }
            let other = name.trim_end_matches(".py");
            if other == stem {
                continue;
            }
            // filter vs filters, aggregate vs aggregator, etc.
            if other.starts_with(stem)
                || stem.starts_with(other)
                || (stem.len() >= 4
                    && other.len() >= 4
                    && (other.contains(stem) || stem.contains(other)))
            {
                hints.push(format!(
                    "{path} may duplicate existing module `{}/{other}.py` — prefer extending one file",
                    parent.display()
                ));
            }
        }
    }
    hints.into_iter().next()
}

fn goal_requires_tests(goal: &str) -> bool {
    let g = goal.to_ascii_lowercase();
    g.contains("unittest")
        || g.contains("pytest")
        || g.contains("cargo test")
        || g.contains("test_*.py")
        || g.contains("tests/")
        || g.contains("write tests")
        || g.contains("add tests")
        || g.contains("test suite")
}

fn goal_is_checkpoint_heal(goal: &str) -> bool {
    let g = goal.to_ascii_lowercase();
    g.contains("checkpoint `") || g.contains("checkpoint failed")
}

fn workspace_python(root: &Path) -> std::path::PathBuf {
    let venv_py = root.join(".venv/bin/python");
    if venv_py.exists() {
        venv_py
    } else {
        std::path::PathBuf::from("python3")
    }
}

/// Pull exact CLI names the request/checkpoint expects to appear in `-h`.
///
/// Sources: `` `backticks` ``, `--flags`, `help_contains:…`, and `-h contains '…'`.
fn extract_cli_help_tokens(goal: &str) -> Vec<String> {
    static BT: OnceLock<Regex> = OnceLock::new();
    static FLAG: OnceLock<Regex> = OnceLock::new();
    static HELP_COLON: OnceLock<Regex> = OnceLock::new();
    static HELP_CONTAINS: OnceLock<Regex> = OnceLock::new();
    let bt = BT.get_or_init(|| Regex::new(r"`([^`]+)`").expect("bt"));
    let flag = FLAG.get_or_init(|| Regex::new(r"(--[a-z][\w-]{1,40})").expect("flag"));
    let help_colon = HELP_COLON.get_or_init(|| {
        Regex::new(r#"(?i)help_contains:\s*['"]?([a-zA-Z][\w-]{1,40})"#).expect("hc")
    });
    let help_contains = HELP_CONTAINS
        .get_or_init(|| Regex::new(r#"(?i)-h contains ['"]([^'"]{1,40})['"]"#).expect("hcontains"));

    let mut out: Vec<String> = Vec::new();
    let push = |out: &mut Vec<String>, raw: &str| {
        let t = raw.trim().trim_matches(|c| c == '\'' || c == '"');
        if !looks_like_cli_help_token(t) {
            return;
        }
        let key = t.to_ascii_lowercase();
        if !out.iter().any(|e| e.eq_ignore_ascii_case(&key)) {
            out.push(t.to_string());
        }
    };

    for cap in bt.captures_iter(goal) {
        let inner = cap.get(1).map(|m| m.as_str()).unwrap_or("").trim();
        // Prefer the first path-free token (e.g. `verify` or `verify --limit`).
        for part in inner.split_whitespace() {
            if looks_like_cli_help_token(part) {
                push(&mut out, part);
            }
        }
    }
    for cap in flag.captures_iter(goal) {
        if let Some(m) = cap.get(1) {
            push(&mut out, m.as_str());
        }
    }
    for cap in help_colon.captures_iter(goal) {
        if let Some(m) = cap.get(1) {
            push(&mut out, m.as_str());
        }
    }
    for cap in help_contains.captures_iter(goal) {
        if let Some(m) = cap.get(1) {
            push(&mut out, m.as_str());
        }
    }
    out.truncate(8);
    out
}

fn looks_like_cli_help_token(tok: &str) -> bool {
    let t = tok.trim();
    if t.len() < 2 || t.len() > 40 {
        return false;
    }
    if t.contains('/') || t.contains('\\') || t.contains('.') {
        return false; // paths / files
    }
    if let Some(rest) = t.strip_prefix("--") {
        return rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    }
    // Subcommand-like bare word; skip common English / env / tooling noise.
    // (Doctor prompts mentioning `pip install` must not become -h token requirements.)
    const STOP: &[&str] = &[
        "the",
        "and",
        "with",
        "from",
        "into",
        "file",
        "each",
        "jobs",
        "job",
        "cli",
        "add",
        "read",
        "runs",
        "must",
        "pass",
        "when",
        "then",
        "that",
        "this",
        "only",
        "true",
        "false",
        "none",
        "null",
        "http",
        "json",
        "markdown",
        "pip",
        "install",
        "python",
        "python3",
        "unittest",
        "discover",
        "pytest",
        "cargo",
        "npm",
        "yarn",
        "poetry",
        "venv",
        "virtualenv",
        "requirements",
        "doctor",
        "help",
        "version",
        "cache",
        "path",
        "module",
        "package",
        "packages",
        "please",
        "should",
        "could",
        "would",
        "using",
        "after",
        "before",
        "under",
    ];
    let lower = t.to_ascii_lowercase();
    if STOP.contains(&lower.as_str()) {
        return false;
    }
    static WORD: OnceLock<Regex> = OnceLock::new();
    let re = WORD.get_or_init(|| Regex::new(r"^[a-z][a-z0-9-]{1,32}$").expect("tok"));
    re.is_match(&lower)
}

/// Prefer a few likely CLI/entrypoint files for heal diagnosis context.
fn heal_context_files(root: &Path, cli_mod: Option<&PythonCliModule>) -> String {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(cli) = cli_mod {
        candidates.push(format!("{}/cli.py", cli.module));
        candidates.push(format!("{}/__main__.py", cli.module));
        candidates.push(format!("{}/__init__.py", cli.module));
    }
    candidates.extend(
        [
            "cli.py",
            "main.py",
            "solution.py",
            "pyproject.toml",
            "README.md",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    );
    let mut out = String::new();
    let mut used = 0usize;
    for rel in candidates {
        if used >= 4 {
            break;
        }
        let path = root.join(&rel);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        out.push_str(&format!("@@@ FILE: {rel}\n{}\n\n", truncate(&text, 1800)));
        used += 1;
    }
    if out.is_empty() {
        "(no key files readable yet)".into()
    } else {
        out
    }
}

fn normalize_heal_plan(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return "Fix the failing check; address every FAIL line in the error output.".into();
    }
    // Prefer the FIX PLAN section when present; keep diagnosis as preamble.
    if let Some(idx) = t.to_ascii_uppercase().find("@@@ FIX PLAN") {
        let diagnosis = t[..idx].trim();
        let plan = t[idx..].trim();
        if diagnosis.is_empty() {
            plan.to_string()
        } else {
            format!("{diagnosis}\n\n{plan}")
        }
    } else {
        t.to_string()
    }
}

fn fail_looks_like_cli_gate(fail: &str) -> bool {
    let f = fail.to_ascii_lowercase();
    f.contains("cli entrypoint")
        || f.contains("help-text gate")
        || f.contains("help_contains")
        || f.contains("python -m ")
        || f.contains(".__main__")
        || f.contains("cannot be directly executed")
        || (f.contains(" -h")
            && (f.contains("module") || f.contains("argparse") || f.contains("usage")))
}

/// Behavioral gate: module `-h` exits 0. Token matching is optional (off for heal; checkpoints own help_contains).
fn verify_python_cli(
    root: &Path,
    cli: &PythonCliModule,
    goal: &str,
    check_tokens: bool,
) -> Result<(), String> {
    let help = run_python_module_help(root, &cli.module)?;
    if !check_tokens {
        return Ok(());
    }
    let tokens = extract_cli_help_tokens(goal);
    if tokens.is_empty() {
        return Ok(());
    }
    let help_lc = help.to_ascii_lowercase();
    let missing: Vec<&str> = tokens
        .iter()
        .filter(|t| !help_lc.contains(&t.to_ascii_lowercase()))
        .map(|s| s.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "`{}` is missing required CLI name(s) from the request: {}.\n\
Use those exact names in argparse (do not rename synonyms).\n\
Help was:\n{}",
            cli.help_display(),
            missing.join(", "),
            truncate(&help, 800)
        ))
    }
}

fn run_python_module_help(root: &Path, module: &str) -> Result<String, String> {
    use std::io::Read;
    let py = workspace_python(root);
    // Prefer workspace package on PYTHONPATH so shadow trees work without editable install.
    let mut child = Command::new(&py)
        .args(["-m", module, "-h"])
        .current_dir(root)
        .env("PYTHONPATH", root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {py:?} -m {module} -h: {e}"))?;
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
    let timeout = Duration::from_secs(30);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err("CLI -h timeout after 30s".into());
                }
                thread::sleep(Duration::from_millis(50));
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
        Ok(combined)
    } else {
        Err(format!(
            "exit {} — {}",
            status.code().unwrap_or(-1),
            truncate(&combined, 1200)
        ))
    }
}

/// Collect protected test file bodies (relative paths) under `root`.
pub fn collect_test_snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    collect_test_snapshot_rec(root, root, &mut out);
    out
}

fn collect_test_snapshot_rec(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if matches!(
                name.as_ref(),
                ".vibe" | ".git" | "target" | "node_modules" | ".venv" | "__pycache__"
            ) {
                continue;
            }
            collect_test_snapshot_rec(root, &path, out);
            continue;
        }
        if !(name.starts_with("test_") && name.ends_with(".py")) && name != "test_solution.py" {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if let Ok(content) = fs::read_to_string(&path) {
            // Skip hollow stubs so we don't "protect" empty placeholders forever.
            if content.trim().len() > 20 {
                out.insert(rel, content);
            }
        }
    }
}

fn restore_protected_tests(shadow: &ShadowWorkspace, snapshot: &BTreeMap<String, String>) {
    write_test_snapshot(shadow.shadow_dir(), snapshot);
    // Also re-copy any still-present origin tests (covers files added after start).
    let origin_snap = collect_test_snapshot(shadow.origin_dir());
    for (path, content) in origin_snap {
        snapshot_get_or_write(shadow.shadow_dir(), &path, &content);
    }
}

fn snapshot_get_or_write(root: &Path, rel: &str, content: &str) {
    let dest = root.join(rel);
    let needs = match fs::read_to_string(&dest) {
        Ok(existing) => existing.trim().len() < 20 || existing != content,
        Err(_) => true,
    };
    if needs {
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&dest, content);
    }
}

fn write_test_snapshot(root: &Path, snapshot: &BTreeMap<String, String>) {
    for (rel, content) in snapshot {
        snapshot_get_or_write(root, rel, content);
    }
}

/// Re-materialize snapshotted tests into a workspace (origin) after agent apply.
pub fn ensure_origin_test_snapshot(origin: &Path, snapshot: &BTreeMap<String, String>) {
    write_test_snapshot(origin, snapshot);
}

fn run_workspace_tests(root: &Path, harness: TestHarness) -> Result<(), String> {
    use std::io::Read;
    let timeout = match harness {
        TestHarness::PythonUnittest => Duration::from_secs(45),
        TestHarness::CargoTest => Duration::from_secs(120),
        TestHarness::None => return Ok(()),
    };
    let mut child = match harness {
        TestHarness::PythonUnittest => {
            let start = if root.join("tests").is_dir() {
                "tests"
            } else {
                "."
            };
            Command::new("python3")
                .args([
                    "-m",
                    "unittest",
                    "discover",
                    "-s",
                    start,
                    "-p",
                    "test_*.py",
                    "-v",
                ])
                .current_dir(root)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?
        }
        TestHarness::CargoTest => Command::new("cargo")
            .args(["test", "--quiet"])
            .current_dir(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?,
        TestHarness::None => return Ok(()),
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
    if !status.success() {
        return Err(combined);
    }
    if harness == TestHarness::PythonUnittest && !unittest_meaningful_pass(&combined) {
        return Err(format!(
            "unittest exited 0 but no active tests ran (all skipped or Ran 0):\n{combined}"
        ));
    }
    Ok(())
}

fn unittest_meaningful_pass(output: &str) -> bool {
    if output.contains("Ran 0 tests") {
        return false;
    }
    // Require at least one actively passed test line (not "... skipped").
    let has_ok_line = output.lines().any(|l| {
        let t = l.trim_end();
        t.ends_with(" ... ok") && !t.contains(" ... skipped")
    });
    if !has_ok_line {
        return false;
    }
    // Pure skip suites: "OK (skipped=N)" with zero "... ok" already rejected above.
    true
}

fn fallback_summary(
    goal: &str,
    touched: &BTreeMap<String, String>,
    reason: &str,
    success: bool,
) -> String {
    let files = touched
        .keys()
        .map(|p| format!("• {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Agent {} for: {}\n\nWhy it stopped: {}\n\nFiles:\n{}\n",
        if success { "finished" } else { "stopped" },
        truncate(goal, 180),
        reason,
        if files.is_empty() {
            "(none)".into()
        } else {
            files
        }
    )
}

fn format_files_written(touched: &BTreeMap<String, String>) -> String {
    if touched.is_empty() {
        "Files written: (none)".into()
    } else {
        format!(
            "Files written ({}):\n{}",
            touched.len(),
            touched
                .keys()
                .map(|p| format!("  • {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

fn truncate(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{t}…")
    } else {
        t
    }
}

/// Prefer assertion / traceback sections so the model sees the real mismatch.
fn distill_test_failure(raw: &str, max: usize) -> String {
    let markers = [
        "\nFAIL: ",
        "\nERROR: ",
        "AssertionError",
        "TypeError",
        "NameError",
        "AttributeError",
        "NotImplementedError",
        "test timeout",
    ];
    let mut start = 0usize;
    for m in markers {
        if let Some(i) = raw.find(m) {
            // Include a little context before the marker when possible.
            let candidate = i.saturating_sub(80);
            if start == 0 || candidate < start {
                start = candidate;
            }
            // Prefer the first strong failure marker.
            if m.starts_with('\n') || m == "AssertionError" {
                start = candidate;
                break;
            }
        }
    }
    let slice = if start > 0 && start < raw.len() {
        &raw[start..]
    } else {
        raw
    };
    truncate(slice.trim(), max)
}

fn sanitize_goal(goal: &str) -> String {
    let cut_markers = [
        "\n### Notes:",
        "\n### Context:",
        "\n### Workspace Tools",
        "\ngit_status:",
        "\nlist_dir:",
    ];
    let mut g = goal.to_string();
    for m in cut_markers {
        if let Some(i) = g.find(m) {
            if m.contains("Workspace") || m.contains("git_status") || m.contains("list_dir") {
                g.truncate(i);
            }
        }
    }
    // Coding problems (LCD) often need the full statement; keep more headroom.
    if g.len() > 8000 {
        g.truncate(8000);
        g.push('…');
    }
    g.trim().to_string()
}

fn status_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)@@@\s*STATUS:\s*(DONE|CONTINUE)").unwrap())
}

fn parse_critique(raw: &str) -> Critique {
    let done = status_re()
        .captures(raw)
        .and_then(|c| c.get(1).map(|m| m.as_str().eq_ignore_ascii_case("DONE")))
        .unwrap_or(false);
    let reason = raw
        .lines()
        .find(|l| l.to_uppercase().contains("@@@ REASON:"))
        .map(|l| {
            l.split_once(':')
                .map(|(_, r)| r.trim().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_else(|| "no reason".into());
    let gaps = if let Some(pos) = raw.to_uppercase().find("@@@ GAPS:") {
        raw[pos..].chars().take(800).collect()
    } else if done {
        String::new()
    } else {
        "Ship the MVP checklist files with a runnable CLI.".into()
    };
    Critique { done, reason, gaps }
}

fn critique_fingerprint(c: &Critique) -> String {
    let r: String = c
        .reason
        .to_lowercase()
        .chars()
        .filter(|ch| ch.is_alphanumeric() || ch.is_whitespace())
        .take(120)
        .collect();
    r.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn observe_agent_files(
    touched: &BTreeMap<String, String>,
    greenfield: bool,
    fallback_path: &str,
    origin_stub: &str,
    required_exports: &[String],
) -> String {
    let mut out = format!("greenfield={greenfield}\n");
    if !required_exports.is_empty() {
        out.push_str(&format!(
            "REQUIRED exports in `{fallback_path}` (exact names): {}\n",
            required_exports.join(", ")
        ));
    }
    if !origin_stub.trim().is_empty() {
        // Do NOT use @@@ FILE here — models copy that into the solution body.
        out.push_str(&format!(
            "\n### Origin stub for `{fallback_path}` (preserve these names; implement the bodies):\n\
--- stub ---\n{}\n--- end stub ---\n\n",
            truncate(origin_stub, 1800)
        ));
    }
    if touched.is_empty() {
        out.push_str("(no agent-written files yet — implement from the stub above)\n");
        return out;
    }
    out.push_str(&format!(
        "### Current agent-written files ({})\n\n",
        touched.len()
    ));
    for (path, content) in touched {
        if is_protected_test_path(path) {
            continue;
        }
        let preview: String = content.chars().take(1200).collect();
        out.push_str(&format!("@@@ FILE: {path}\n{preview}"));
        if content.chars().count() > 1200 {
            out.push_str("\n…(truncated)\n\n");
        } else {
            out.push_str("\n\n");
        }
    }
    out
}

fn extract_required_python_exports(origin: &Path) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"from\s+solution\s+import\s+([^\n]+)").expect("import regex"));
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(origin) else {
        return names;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("test_") && name.ends_with(".py")) {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        for caps in re.captures_iter(&text) {
            let list = caps.get(1).map(|m| m.as_str()).unwrap_or("").trim();
            let list = list.trim_matches(|c| c == '(' || c == ')' || c == '\\');
            for part in list.split(',') {
                let token = part
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c| c == '(' || c == ')' || c == '\\' || c == ',');
                if token.is_empty() || token == "*" || token == "\\" {
                    continue;
                }
                // `foo as bar` → keep foo (exported name from solution)
                let export = token.split_whitespace().next().unwrap_or(token);
                if export
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    names.push(export.to_string());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn read_origin_stub(origin: &Path, fallback_path: &str) -> String {
    let path = origin.join(fallback_path);
    fs::read_to_string(path).unwrap_or_default()
}

fn validate_python_exports(content: &str, required: &[String]) -> Result<(), String> {
    let mut missing = Vec::new();
    for name in required {
        let def_fn = format!("def {name}");
        let async_fn = format!("async def {name}");
        let class = format!("class {name}");
        let assign_re = Regex::new(&format!(r"(?m)^\s*{}\s*=", regex::escape(name))).ok();
        let ok = content.contains(&def_fn)
            || content.contains(&async_fn)
            || content.contains(&class)
            || assign_re
                .as_ref()
                .map(|r| r.is_match(content))
                .unwrap_or(false);
        if !ok {
            missing.push(name.clone());
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "missing required exports: {}. Keep the exact names from the stub/tests (do not rename to a Solution class unless tests import Solution).",
            missing.join(", ")
        ))
    }
}

fn revert_shadow_writes(
    shadow: &ShadowWorkspace,
    snapshot: &BTreeMap<String, String>,
    attempted_paths: &[String],
    test_snapshot: &BTreeMap<String, String>,
) -> Result<()> {
    for path in attempted_paths {
        if let Some(prev) = snapshot.get(path) {
            shadow.stage_file_change(path, prev)?;
        } else {
            let origin = shadow.origin_dir().join(path);
            if origin.exists() {
                let content = fs::read_to_string(&origin).unwrap_or_default();
                shadow.stage_file_change(path, &content)?;
            }
        }
    }
    // Re-stage entire snapshot so shadow matches last good agent state.
    for (path, content) in snapshot {
        shadow.stage_file_change(path, content)?;
    }
    restore_protected_tests(shadow, test_snapshot);
    Ok(())
}

fn looks_like_mvp(touched: &BTreeMap<String, String>) -> bool {
    if touched.is_empty() {
        return false;
    }
    let has_entrypoint = touched.keys().any(|p| {
        let name = Path::new(p)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        matches!(
            name,
            "cli.py" | "main.py" | "app.py" | "__main__.py" | "index.js" | "main.rs" | "main.go"
        ) || p.ends_with("/cli.py")
            || p.ends_with("/main.py")
    });
    let has_code = touched.values().any(|c| {
        c.contains("def ")
            || c.contains("function ")
            || c.contains("fn ")
            || c.contains("argparse")
            || c.contains("click")
    });
    let non_empty = touched.values().filter(|c| c.trim().len() > 40).count() >= 1;
    (has_entrypoint && has_code) || (non_empty && touched.len() >= 2 && has_code)
}

fn stage_and_record(
    shadow: &ShadowWorkspace,
    files: &[ParsedFile],
    touched: &mut BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let mut staged = Vec::new();
    for f in files {
        if is_protected_test_path(&f.path) {
            continue;
        }
        let content = scrub_agent_artifacts(&f.content);
        if content.trim().is_empty() {
            continue;
        }
        shadow.stage_file_change(&f.path, &content)?;
        touched.insert(f.path.clone(), content);
        staged.push(f.path.clone());
    }
    Ok(staged)
}

fn harness_plan(harness: TestHarness, fallback_path: &str) -> String {
    match harness {
        TestHarness::PythonUnittest => format!(
            "- `{fallback_path}` — implement the required Solution / functions so existing tests pass\n\
- Do NOT edit `test_*.py`\n\
- Do NOT add README or extra files\n\
- `python3 -m unittest discover -s . -p 'test_*.py' -v` must pass"
        ),
        TestHarness::CargoTest => format!(
            "- `{fallback_path}` — implement so `cargo test` passes\n\
- Do NOT gut existing tests\n\
- Keep the crate building"
        ),
        TestHarness::None => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    WrongAnswer,
    RuntimeError,
    NameOrImport,
    Timeout,
    RejectedWrite,
    Other,
}

fn classify_test_failure(test_fail: &str) -> FailureClass {
    let t = test_fail.to_lowercase();
    if t.contains("rejected")
        || t.contains("missing required export")
        || t.contains("empty write")
        || t.contains("wrong-api")
    {
        return FailureClass::RejectedWrite;
    }
    if t.contains("test timeout") || t.contains("timed out") {
        return FailureClass::Timeout;
    }
    if t.contains("nameerror")
        || t.contains("importerror")
        || t.contains("modulenotfounderror")
        || t.contains("cannot import name")
        || t.contains("is not defined")
    {
        return FailureClass::NameOrImport;
    }
    // Prefer concrete runtime exceptions over generic "error" in tracebacks.
    if t.contains("valueerror")
        || t.contains("indexerror")
        || t.contains("keyerror")
        || t.contains("typeerror")
        || t.contains("zerodivisionerror")
        || t.contains("attributeerror")
        || t.contains("recursionerror")
        || t.contains("overflowerror")
        || t.contains("iterable argument is empty")
        || t.contains("list index out of range")
    {
        return FailureClass::RuntimeError;
    }
    if (t.contains("got ") && t.contains("expected "))
        || t.contains("assertionerror")
        || t.contains("failed (failures")
    {
        return FailureClass::WrongAnswer;
    }
    if t.contains("error:") || t.contains("traceback") {
        return FailureClass::RuntimeError;
    }
    FailureClass::Other
}

/// Generic repair playbooks — failure-class based, not problem-specific.
fn repair_playbook(class: FailureClass) -> &'static str {
    match class {
        FailureClass::WrongAnswer => {
            "Failure class: WRONG ANSWER (tests ran; output mismatch).\n\
Debug checklist:\n\
1. Mentally execute the failing input from the test output step-by-step; change the code so that input yields the expected value.\n\
2. Re-read the ask carefully: substring vs whole string, \"any\" vs \"all\", count vs exists, exact order vs any order.\n\
3. If got is 0 / False / empty / None but expected is positive / True / non-empty: your filter is too strict or you miss valid cases — enumerate all allowed starts, directions, splits, or configurations; do not exit early when one branch fails.\n\
4. Special tokens (e.g. a single wildcard) often allow an empty replacement; left/right parts may match with nothing in the middle.\n\
5. Simulation / search: copy state per branch; mutating one shared array/path across attempts undercounts.\n\
6. Boundaries: empty match, single element, all equal, zeros, negatives, duplicates, inclusive index ranges.\n\
7. Prefer a clear correct approach over a clever broken one; rewrite the body if needed.\n\
8. Do not change signatures or invent APIs the tests do not call."
        }
        FailureClass::RuntimeError => {
            "Failure class: RUNTIME ERROR (crash before/during assertions).\n\
Debug checklist:\n\
1. Read the exception type and line — fix that exact crash (empty min/max, missing key, bad index, wrong type).\n\
2. Never call min/max/next on a filtered list/set without an emptiness check — filtering by parity, sign, or predicate often leaves a category empty.\n\
3. When a category is empty, return a sentinel consistent with the return type and tests (common: 0, -1, False, [], or float('-inf') / float('inf')), not a crash. Do this even if the prose says a case is \"guaranteed\"; tests may still include edges.\n\
4. Keep imports from the stub/prompt; do not call undefined names.\n\
5. After the crash is gone, re-check logic against examples and any got/expected still shown."
        }
        FailureClass::NameOrImport => {
            "Failure class: NAME / IMPORT ERROR.\n\
Debug checklist:\n\
1. Define every name the traceback says is missing (or import it from the stdlib).\n\
2. Keep REQUIRED exports and signatures exactly as in the stub/tests.\n\
3. Preserve the import block from the origin stub when present (typing, collections, etc.).\n\
4. Do not wrap the solution in a different class/module layout than tests expect."
        }
        FailureClass::Timeout => {
            "Failure class: TIMEOUT (likely infinite loop or too-slow algorithm).\n\
Debug checklist:\n\
1. Remove unbounded while-loops; ensure every loop makes progress.\n\
2. Replace exponential / nested full scans with the intended complexity from constraints (often O(n) or O(n log n)).\n\
3. Avoid recomputing the same work inside inner loops without need.\n\
4. Re-check termination conditions on the failing or largest plausible inputs."
        }
        FailureClass::RejectedWrite => {
            "Failure class: INVALID WRITE (empty file, scrubbed markers, or missing required API).\n\
Debug checklist:\n\
1. Emit only valid source for the target file — no @@@ markers, markdown, or commentary in the body.\n\
2. Keep exact required export names from the stub/tests.\n\
3. Start from the stub signatures and fill in working bodies."
        }
        FailureClass::Other => {
            "Failure class: UNSPECIFIED TEST FAILURE.\n\
Debug checklist:\n\
1. Read the full traceback/test output carefully.\n\
2. Fix the root cause, preserve required APIs, re-emit the complete file."
        }
    }
}

fn rewrite_from_scratch_playbook(class: FailureClass) -> String {
    format!(
        "Failure class: {class:?} — REWRITE FROM SCRATCH (same failure class twice already; prior approach is wrong).\n\
Instructions:\n\
1. IGNORE any previous implementation ideas — do not patch the old algorithm.\n\
2. Keep ONLY the required signatures / class from the stub.\n\
3. Design a fresh solution aimed at the failing example (see probe / focus below).\n\
4. Still follow the usual edge-case discipline for this failure class (empty filters, substring vs whole, undercounting, etc.).\n\
5. Emit a complete new file body."
    )
}

fn build_repair_prompt(
    goal: &str,
    observation: &str,
    test_fail: &str,
    fallback_path: &str,
    required_exports: &[String],
    force_rewrite: bool,
    probe: Option<&str>,
    static_hints: Option<&str>,
    debug_advice: Option<&str>,
) -> String {
    let class = classify_test_failure(test_fail);
    let playbook = if force_rewrite {
        rewrite_from_scratch_playbook(class)
    } else {
        repair_playbook(class).to_string()
    };
    let first = first_failure_focus(test_fail);
    let probe_block = match probe {
        Some(p) if !p.trim().is_empty() => format!(
            "\n### Failing-case probe (executed your current code on the failing call):\n{p}\n\
Use this: make the code produce PROBE_EXPECTED (or stop throwing PROBE_EXC) for this exact call.\n"
        ),
        _ => String::new(),
    };
    let static_block = match static_hints {
        Some(h) if !h.trim().is_empty() => format!(
            "\n### Static diagnosis (AST + rules — not a guess):\n{h}\n\
Apply every FIX hint that matches; do not ignore FACTS that contradict your approach.\n"
        ),
        _ => String::new(),
    };
    let debug_block = match debug_advice {
        Some(d) if !d.trim().is_empty() => format!(
            "\n### Debug-model diagnosis (follow unless it contradicts probe/static facts):\n{d}\n"
        ),
        _ => String::new(),
    };
    let exports = if required_exports.is_empty() {
        String::new()
    } else {
        format!(
            "\n- REQUIRED exports (must remain defined): {}\n",
            required_exports.join(", ")
        )
    };
    eprintln!(
        "[vibe] agent repair playbook: {class:?} rewrite={force_rewrite} probe={} static={} debug={}",
        probe.is_some(),
        static_hints.is_some(),
        debug_advice.is_some()
    );
    format!(
        r#"You are fixing a coding solution so unit tests pass.

### Problem:
{goal}

### Focus on this failure first:
{first}
{probe_block}{static_block}{debug_block}
### Repair playbook (follow this):
{playbook}

### Full test output:
{test_fail}

### Current files / required stub:
{observation}

Rules:
- Emit ONLY: @@@ FILE: {fallback_path}
- Then the COMPLETE corrected file contents.
- Valid source code ONLY inside the file — never put `@@@`, markdown fences, or commentary inside the file body.
- Keep class/method/function signatures exactly as required by tests.{exports}- Address the failure class above; do not ignore the concrete got/expected, probe, static FIX hints, debug diagnosis, or exception.
- Do not edit test files. Do not add README.

Output:
@@@ FILE: {fallback_path}
<full corrected source>
"#
    )
}

fn build_debug_diagnose_prompt(
    goal: &str,
    test_fail: &str,
    class: FailureClass,
    probe: Option<&str>,
    static_hints: Option<&str>,
    observation: &str,
    force_rewrite: bool,
) -> String {
    let probe = probe.unwrap_or("(no probe)");
    let static_hints = static_hints.unwrap_or("(no static facts)");
    let rewrite = if force_rewrite {
        "Prior approach failed repeatedly — prefer a different algorithm."
    } else {
        "A targeted patch is OK if the root cause is clear."
    };
    format!(
        r#"You are a debugging assistant for a coding agent. Do NOT write the full solution file.
Diagnose why the current code fails the tests and give concrete fix instructions for another model that will edit the code.

Failure class: {class:?}
{rewrite}

### Problem (may be truncated):
{goal}

### Probe (executed failing call):
{probe}

### Static facts / FIX hints:
{static_hints}

### Test failure (distilled):
{test_fail}

### Current code / stub:
{observation}

Respond with ONLY:
1. ROOT CAUSE: one or two sentences
2. WHY GOT ≠ EXPECTED (or why the exception happens)
3. CONCRETE FIX: 3–6 bullet steps the coder must implement
4. PITFALLS: anything to avoid (wrong polarity, empty min/max, single vs multi wildcard, etc.)

No markdown code fences unless a 1–3 line snippet is essential. No @@@ FILE markers. No full rewritten file."#,
        goal = truncate(goal, 3500),
        test_fail = truncate(test_fail, 900),
        observation = truncate(observation, 2500),
    )
}

/// Strip reasoning-model wrappers and keep actionable text for the coder model.
fn scrub_reasoning_output(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    // DeepSeek-R1 style: <think> ... </think> (also accept "thinking"/"redacted_reasoning").
    static RE_THINK: OnceLock<Regex> = OnceLock::new();
    let re = RE_THINK.get_or_init(|| {
        Regex::new(
            r"(?is)<(?:think|thinking|redacted_thinking|redacted_reasoning)>.*?</(?:think|thinking|redacted_thinking|redacted_reasoning)>",
        )
        .expect("think tag regex")
    });
    s = re.replace_all(&s, "").to_string();
    for marker in ["@@@ FILE:", "@@@ STATUS:"] {
        if let Some(i) = s.find(marker) {
            s.truncate(i);
        }
    }
    let t = s.trim();
    let t = t
        .strip_prefix("```")
        .map(|x| match x.find('\n') {
            Some(nl) => &x[nl + 1..],
            None => x,
        })
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    t.trim().to_string()
}

#[derive(Debug, Default, Clone)]
struct PythonSolutionFacts {
    methods: Vec<String>,
    calls_min: bool,
    calls_max: bool,
    min_max_unguarded: bool,
    uses_counter: bool,
    returns_not: bool,
    splits_on_star: bool,
    has_inf_sentinel: bool,
    has_while_true: bool,
    nested_loops: u32,
    imports: Vec<String>,
    parse_error: Option<String>,
}

fn analyze_python_solution(path: &Path) -> Option<PythonSolutionFacts> {
    let script = r#"
import ast, json, sys
path = sys.argv[1]
src = open(path, encoding="utf-8").read()
out = {
  "methods": [], "calls_min": False, "calls_max": False, "min_max_unguarded": False,
  "uses_counter": False, "returns_not": False, "splits_on_star": False,
  "has_inf_sentinel": False, "has_while_true": False, "nested_loops": 0,
  "imports": [], "parse_error": None,
}
try:
    tree = ast.parse(src)
except SyntaxError as e:
    out["parse_error"] = f"SyntaxError: {e}"
    print(json.dumps(out))
    raise SystemExit(0)

class V(ast.NodeVisitor):
    def __init__(self):
        self.loop_depth = 0
        self.max_loop = 0
        self.func_stack = []
    def visit_Import(self, n):
        for a in n.names:
            out["imports"].append(a.name.split(".")[0])
        self.generic_visit(n)
    def visit_ImportFrom(self, n):
        if n.module:
            out["imports"].append(n.module.split(".")[0])
        for a in n.names:
            if a.name == "Counter":
                out["uses_counter"] = True
        self.generic_visit(n)
    def visit_FunctionDef(self, n):
        self.func_stack.append(n.name)
        out["methods"].append(n.name)
        self.generic_visit(n)
        self.func_stack.pop()
    visit_AsyncFunctionDef = visit_FunctionDef
    def visit_For(self, n):
        self.loop_depth += 1
        self.max_loop = max(self.max_loop, self.loop_depth)
        self.generic_visit(n)
        self.loop_depth -= 1
    visit_AsyncFor = visit_For
    def visit_While(self, n):
        if isinstance(n.test, ast.Constant) and n.test.value is True:
            out["has_while_true"] = True
        self.loop_depth += 1
        self.max_loop = max(self.max_loop, self.loop_depth)
        self.generic_visit(n)
        self.loop_depth -= 1
    def visit_Return(self, n):
        if isinstance(n.value, ast.UnaryOp) and isinstance(n.value.op, ast.Not):
            out["returns_not"] = True
        self.generic_visit(n)
    def visit_Call(self, n):
        name = None
        if isinstance(n.func, ast.Name):
            name = n.func.id
        elif isinstance(n.func, ast.Attribute):
            name = n.func.attr
            if isinstance(n.func.value, ast.Name) and n.func.value.id == "collections" and n.func.attr == "Counter":
                out["uses_counter"] = True
        if name == "min":
            out["calls_min"] = True
            out["min_max_unguarded"] = True
        elif name == "max":
            out["calls_max"] = True
            out["min_max_unguarded"] = True
        elif name == "Counter":
            out["uses_counter"] = True
        elif name in ("split", "partition", "rsplit", "rpartition"):
            if n.args and isinstance(n.args[0], ast.Constant) and n.args[0].value == "*":
                out["splits_on_star"] = True
        self.generic_visit(n)
    def visit_Name(self, n):
        if n.id == "inf":
            out["has_inf_sentinel"] = True
        self.generic_visit(n)

v = V()
v.visit(tree)
out["nested_loops"] = v.max_loop
# Guard heuristic: if min/max line is preceded (within 4 lines) by `if not` / `if len` / `or []` / `or ()`
lines = src.splitlines()
for i, line in enumerate(lines):
    if "min(" in line or "max(" in line:
        window = "\n".join(lines[max(0, i-4):i+1]).lower()
        if any(tok in window for tok in ("if not ", "if len(", " or []", " or ()", " or set()", "else float", "-inf", "inf")):
            out["min_max_unguarded"] = False
            break
if "float(\"inf\")" in src or "float('inf')" in src or "float(\"-inf\")" in src or "float('-inf')" in src:
    out["has_inf_sentinel"] = True
if ".split(\"*\")" in src or ".split('*')" in src or ".partition(\"*\")" in src or ".partition('*')" in src:
    out["splits_on_star"] = True
print(json.dumps(out))
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(path.as_os_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() && output.stdout.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    Some(PythonSolutionFacts {
        methods: v
            .get("methods")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        calls_min: v
            .get("calls_min")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        calls_max: v
            .get("calls_max")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        min_max_unguarded: v
            .get("min_max_unguarded")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        uses_counter: v
            .get("uses_counter")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        returns_not: v
            .get("returns_not")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        splits_on_star: v
            .get("splits_on_star")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        has_inf_sentinel: v
            .get("has_inf_sentinel")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        has_while_true: v
            .get("has_while_true")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        nested_loops: v.get("nested_loops").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        imports: v
            .get("imports")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        parse_error: v
            .get("parse_error")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    })
}

fn build_static_fix_hints(
    shadow_root: &Path,
    fallback_path: &str,
    class: FailureClass,
    test_fail: &str,
    probe: Option<&str>,
) -> Option<String> {
    let sol = shadow_root.join(fallback_path);
    let sol = if sol.exists() {
        sol
    } else {
        shadow_root.join("solution.py")
    };
    if !sol.exists() {
        return None;
    }
    let facts = analyze_python_solution(&sol)?;
    let probe_txt = probe.unwrap_or("");
    let mut facts_lines = Vec::new();
    if let Some(ref e) = facts.parse_error {
        facts_lines.push(format!("parse_error={e}"));
    }
    if !facts.methods.is_empty() {
        facts_lines.push(format!("methods=[{}]", facts.methods.join(", ")));
    }
    let mut flags = Vec::new();
    if facts.calls_min {
        flags.push("calls_min");
    }
    if facts.calls_max {
        flags.push("calls_max");
    }
    if facts.min_max_unguarded {
        flags.push("min_max_unguarded");
    }
    if facts.uses_counter {
        flags.push("uses_Counter");
    }
    if facts.returns_not {
        flags.push("returns_not");
    }
    if facts.splits_on_star {
        flags.push("splits_on_star");
    }
    if facts.has_inf_sentinel {
        flags.push("has_inf_sentinel");
    }
    if facts.has_while_true {
        flags.push("while_True");
    }
    if facts.nested_loops >= 2 {
        flags.push("nested_loops");
    }
    if !flags.is_empty() {
        facts_lines.push(format!("flags=[{}]", flags.join(", ")));
    }
    if !facts.imports.is_empty() {
        let mut im = facts.imports.clone();
        im.sort();
        im.dedup();
        facts_lines.push(format!("imports=[{}]", im.join(", ")));
    }

    let mut hints = Vec::new();
    let probe_l = probe_txt.to_lowercase();
    let fail_l = test_fail.to_lowercase();
    let empty_min = probe_l.contains("iterable argument is empty")
        || fail_l.contains("iterable argument is empty");
    let expected_inf = (probe_l.contains("probe_expected")
        && (probe_l.contains("-inf") || probe_l.contains("inf")))
        || extract_expected_value(test_fail)
            .map(|e| e.contains("inf"))
            .unwrap_or(false);

    if facts.parse_error.is_some() {
        hints.push(
            "FIX: solution.py has a SyntaxError — rewrite a complete valid file from the stub signatures."
                .into(),
        );
    }
    if empty_min || (facts.min_max_unguarded && class == FailureClass::RuntimeError) {
        hints.push(
            "FIX: min()/max() on a filtered collection that can be empty. Guard emptiness; when a required category is empty, return a sentinel matching PROBE_EXPECTED / tests (often float('-inf') or float('inf')), never crash."
                .into(),
        );
    }
    if expected_inf && !facts.has_inf_sentinel {
        hints.push(
            "FIX: expected value involves ±inf but code has no inf sentinel — import/define float('-inf')/float('inf') and return it for impossible / empty-category cases."
                .into(),
        );
    }
    if class == FailureClass::WrongAnswer && facts.returns_not {
        hints.push(
            "FIX: code uses `return not ...`. Polarity may be wrong — make the probe call return PROBE_EXPECTED directly; do not flip unless the spec is a negative predicate."
                .into(),
        );
    }
    // Multi-star / wildcard patterns in failing call but no split on '*'.
    let call_has_star = probe_txt.contains('*')
        || test_fail.contains("p =") && test_fail.contains('*')
        || extract_candidate_call_args(test_fail)
            .map(|a| a.contains('*'))
            .unwrap_or(false);
    if call_has_star && !facts.splits_on_star && class == FailureClass::WrongAnswer {
        hints.push(
            "FIX: failing input includes '*' but code never split/partition on '*'. Treat each '*' as a flexible gap (possibly empty); match left/right (or segments) as substrings in order — tests may use more than one '*'."
                .into(),
        );
    }
    if class == FailureClass::NameOrImport {
        if let Some(name) =
            extract_undefined_name(test_fail).or_else(|| extract_undefined_name(probe_txt))
        {
            hints.push(format!(
                "FIX: define or import `{name}` (stdlib import or local helper). Keep required Solution exports."
            ));
        } else {
            hints.push(
                "FIX: resolve the NameError/ImportError from the traceback; keep stub imports and required exports."
                    .into(),
            );
        }
    }
    if class == FailureClass::Timeout || facts.has_while_true {
        hints.push(
            "FIX: ensure every loop terminates; avoid `while True` without a clear progress/break; prefer O(n)/O(n log n) over exponential search when constraints are small-but-nested."
                .into(),
        );
    }
    if class == FailureClass::WrongAnswer
        && probe_l.contains("probe_got")
        && probe_l.contains("probe_expected")
    {
        hints.push(
            "FIX: change the algorithm so this exact PROBE_CALL yields PROBE_EXPECTED; prefer a full rewrite of the method body if the current approach cannot."
                .into(),
        );
    }
    if facts.uses_counter
        && (empty_min || facts.min_max_unguarded)
        && (class == FailureClass::RuntimeError || class == FailureClass::WrongAnswer)
    {
        hints.push(
            "FIX: with Counter/frequencies, partition into odd vs even (or other filters) safely — a side can be empty even when the prompt says otherwise."
                .into(),
        );
    }

    if facts_lines.is_empty() && hints.is_empty() {
        return None;
    }
    let mut block = String::new();
    if !facts_lines.is_empty() {
        block.push_str("FACTS:\n");
        for l in &facts_lines {
            block.push_str("- ");
            block.push_str(l);
            block.push('\n');
        }
    }
    if !hints.is_empty() {
        block.push_str("FIX hints:\n");
        for (i, h) in hints.iter().enumerate() {
            block.push_str(&format!("{}. {}\n", i + 1, h));
        }
    }
    Some(block)
}

fn extract_undefined_name(text: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"name '([^']+)' is not defined").expect("nameerror regex"));
    re.captures(text)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// Re-run the first failing `candidate(...)` call against the current shadow solution.
fn probe_failing_python_case(shadow_root: &Path, test_fail: &str) -> Option<String> {
    let call_args = extract_candidate_call_args(test_fail)
        .or_else(|| lookup_failing_call_from_test_file(shadow_root, test_fail))?;
    let expected_expr = extract_expected_value(test_fail)
        .or_else(|| lookup_expected_from_test_file(shadow_root, &call_args))
        .unwrap_or_else(|| "None".into());
    let binder = read_candidate_binder(shadow_root).unwrap_or_else(|| "Solution()".into());
    // binder / call_args / expected_expr are embedded as Python source fragments.
    let call_label = format!("candidate({call_args})");
    let script = format!(
        r#"
import sys
sys.path.insert(0, ".")
inf = float("inf")
nan = float("nan")
_expected = {expected_expr}
print("PROBE_CALL", {call_label:?})
print("PROBE_EXPECTED", repr(_expected))
try:
    import solution as _sol
except Exception as e:
    print("PROBE_EXC", "ImportError:", e)
    raise SystemExit(0)
ns = {{"Solution": getattr(_sol, "Solution", None), "inf": inf, "nan": nan}}
ns.update({{k: getattr(_sol, k) for k in dir(_sol) if not k.startswith("_")}})
try:
    candidate = eval({binder:?}, ns, ns)
except Exception as e:
    print("PROBE_EXC", "BindError:", e)
    raise SystemExit(0)
try:
    got = candidate({call_args})
    print("PROBE_GOT", repr(got))
except Exception as e:
    print("PROBE_EXC", type(e).__name__ + ":", e)
"#
    );
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(&script)
        .current_dir(shadow_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let timeout = Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Some(format!(
                        "PROBE_EXC Timeout: probe exceeded {}s\nPROBE_CALL candidate({call_args})\nPROBE_EXPECTED {expected_expr}",
                        timeout.as_secs()
                    ));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let trimmed = combined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate(trimmed, 800))
    }
}

fn extract_candidate_call_args(test_fail: &str) -> Option<String> {
    static RE_GOT: OnceLock<Regex> = OnceLock::new();
    static RE_ASSERT: OnceLock<Regex> = OnceLock::new();
    static RE_MSG: OnceLock<Regex> = OnceLock::new();
    let re_got = RE_GOT
        .get_or_init(|| Regex::new(r"_got\s*=\s*candidate\((.+)\)\s*$").expect("got call regex"));
    let re_assert = RE_ASSERT
        .get_or_init(|| Regex::new(r"assert\s+candidate\((.+)\)\s*==").expect("assert call regex"));
    // AssertionError: candidate(...) got X expected Y
    let re_msg =
        RE_MSG.get_or_init(|| Regex::new(r"candidate\((.+)\)\s+got\s+").expect("msg call regex"));
    for line in test_fail.lines() {
        let t = line.trim();
        if let Some(c) = re_got.captures(t) {
            return Some(c.get(1)?.as_str().trim().to_string());
        }
        if let Some(c) = re_assert.captures(t) {
            return Some(c.get(1)?.as_str().trim().to_string());
        }
        if let Some(c) = re_msg.captures(t) {
            return Some(c.get(1)?.as_str().trim().to_string());
        }
    }
    // Fallback: any candidate(...)
    static RE_ANY: OnceLock<Regex> = OnceLock::new();
    let re_any =
        RE_ANY.get_or_init(|| Regex::new(r"candidate\((.+)\)").expect("any candidate regex"));
    for line in test_fail.lines() {
        if let Some(c) = re_any.captures(line) {
            return Some(c.get(1)?.as_str().trim().to_string());
        }
    }
    None
}

fn extract_expected_value(test_fail: &str) -> Option<String> {
    static RE_MSG: OnceLock<Regex> = OnceLock::new();
    static RE_EXP: OnceLock<Regex> = OnceLock::new();
    let re_msg =
        RE_MSG.get_or_init(|| Regex::new(r"(?i)expected\s+(.+?)\s*$").expect("expected msg regex"));
    let re_exp =
        RE_EXP.get_or_init(|| Regex::new(r"_exp\s*=\s*(.+?)\s*$").expect("exp assign regex"));
    for line in test_fail.lines() {
        let t = line.trim();
        if t.to_lowercase().contains("got ") && t.to_lowercase().contains("expected ") {
            // Skip source lines that still contain the f-string placeholder.
            if t.contains("{_exp") || t.contains("{_got") {
                continue;
            }
            if let Some(c) = re_msg.captures(t) {
                let v = c.get(1)?.as_str().trim().trim_matches('\'');
                if v.contains('{') {
                    continue;
                }
                // AssertionError: got 0 expected 6  OR  got False expected True
                return Some(normalize_expected_literal(v));
            }
        }
        if let Some(c) = re_exp.captures(t) {
            let v = c.get(1)?.as_str().trim();
            if v.contains('{') {
                continue;
            }
            return Some(normalize_expected_literal(v));
        }
    }
    None
}

/// When the traceback only shows `raise AssertionError`, recover call args via line no.
fn lookup_failing_call_from_test_file(shadow_root: &Path, test_fail: &str) -> Option<String> {
    static RE_LINE: OnceLock<Regex> = OnceLock::new();
    let re_line = RE_LINE
        .get_or_init(|| Regex::new(r#"test_solution\.py", line (\d+)"#).expect("tb line regex"));
    let mut line_no: Option<usize> = None;
    for line in test_fail.lines() {
        if line.contains("in check") {
            if let Some(c) = re_line.captures(line) {
                line_no = c.get(1)?.as_str().parse().ok();
            }
        }
    }
    let text = fs::read_to_string(shadow_root.join("test_solution.py")).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    if let Some(n) = line_no {
        // Traceback line is 1-indexed; look back a few lines for `_got = candidate(...)`.
        let idx = n.saturating_sub(1).min(lines.len().saturating_sub(1));
        let start = idx.saturating_sub(6);
        for line in lines[start..=idx].iter().rev() {
            if let Some(args) = extract_candidate_call_args(line) {
                return Some(args);
            }
        }
    }
    // First enriched assert in the file as a weak fallback.
    for line in &lines {
        if let Some(args) = extract_candidate_call_args(line) {
            return Some(args);
        }
    }
    None
}

fn lookup_expected_from_test_file(shadow_root: &Path, call_args: &str) -> Option<String> {
    let text = fs::read_to_string(shadow_root.join("test_solution.py")).ok()?;
    let needle = format!("candidate({call_args})");
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.contains(&needle) {
            for j in i..=(i + 3).min(lines.len().saturating_sub(1)) {
                if let Some(v) = extract_expected_value(lines[j]) {
                    return Some(v);
                }
                let t = lines[j].trim();
                if let Some(rest) = t.strip_prefix("_exp =") {
                    return Some(normalize_expected_literal(rest.trim()));
                }
            }
        }
    }
    None
}

fn normalize_expected_literal(raw: &str) -> String {
    let t = raw.trim().trim_end_matches(',').trim();
    if t == "True" || t == "False" || t == "None" {
        return t.to_string();
    }
    if t == "-inf" || t == "inf" {
        return format!("float('{t}')");
    }
    // Already a Python literal (number, list, quoted string)?
    if t.starts_with('[')
        || t.starts_with('{')
        || t.starts_with('(')
        || t.starts_with('"')
        || t.starts_with('\'')
        || t.parse::<i64>().is_ok()
        || t.parse::<f64>().is_ok()
    {
        return t.to_string();
    }
    format!("{t:?}") // quote as string
}

fn read_candidate_binder(shadow_root: &Path) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"check\((.+)\)").expect("check binder"));
    let test_path = shadow_root.join("test_solution.py");
    let text = fs::read_to_string(test_path).ok()?;
    // Prefer the call site `check(Solution().foo)`, not `def check(candidate):`.
    let mut last = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("def ") {
            continue;
        }
        if let Some(c) = re.captures(t) {
            let inner = c.get(1)?.as_str().trim();
            if !inner.is_empty() && inner != "candidate" {
                last = Some(inner.to_string());
            }
        }
    }
    last
}

fn first_failure_focus(test_fail: &str) -> String {
    let lines: Vec<&str> = test_fail.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let lower = t.to_lowercase();
        if t.starts_with("assert ")
            || (t.contains("got ") && t.contains("expected "))
            || t.starts_with("AssertionError")
            || t.starts_with("_got =")
            || lower.starts_with("valueerror")
            || lower.starts_with("indexerror")
            || lower.starts_with("keyerror")
            || lower.starts_with("typeerror")
            || lower.starts_with("nameerror")
            || lower.starts_with("attributeerror")
            || lower.contains("error:") && i + 1 < lines.len()
        {
            let start = i.saturating_sub(2);
            let end = (i + 5).min(lines.len());
            return truncate(&lines[start..end].join("\n"), 600);
        }
    }
    truncate(test_fail, 500)
}

fn touched_to_files(touched: &BTreeMap<String, String>) -> Vec<ParsedFile> {
    touched
        .iter()
        .filter(|(p, _)| !is_protected_test_path(p))
        .map(|(path, content)| ParsedFile {
            path: path.clone(),
            content: content.clone(),
        })
        .collect()
}

fn build_act_prompt(
    goal: &str,
    plan: &str,
    observation: &str,
    critique: &Critique,
    greenfield: bool,
    turn: u32,
    harness: TestHarness,
    required_exports: &[String],
    small_or_heal: bool,
    cli_mod: Option<&PythonCliModule>,
) -> String {
    let mode = if greenfield {
        "GREENFIELD: write new files under a dedicated folder (e.g. scripts/job_search/). Do NOT touch unrelated existing project files."
    } else {
        "Modify/create only files needed for the checklist."
    };
    let export_rule = if required_exports.is_empty() {
        String::new()
    } else {
        format!(
            "\n- You MUST define these exact names (tests import them): {}.",
            required_exports.join(", ")
        )
    };
    let test_rules = match harness {
        TestHarness::None => String::new(),
        TestHarness::PythonUnittest => {
            format!(
                "\n- Do NOT create/edit/delete `test_*.py` — those are the scoring harness.\n\
- Implement from the origin stub signatures — do not invent a different API.{export_rule}\n\
- Prefer editing `solution.py` only until `python3 -m unittest discover` passes."
            )
        }
        TestHarness::CargoTest => {
            "\n- Do NOT gut existing tests; make `cargo test` pass by fixing implementation code."
                .into()
        }
    };
    format!(
        r#"You are a pragmatic coding agent. Implement the MVP. Do not over-engineer.

{mode}
Turn: {turn}

### User request:
{goal}

### MVP checklist:
{plan}

### Gaps to close now:
{}
({})

### Workspace / stub / current files:
{observation}

Rules:
- Emit COMPLETE file contents with @@@ FILE: path markers.
- Prefer a working CLI/script over abstract "Integration" / "Engine" classes.
- If web/job search is requested: implement real HTTP calls (requests/httpx) to public job APIs or search endpoints, with clear CLI usage.
- Keep the package small (typically 2–5 files).
- Extend existing modules; do NOT create parallel duplicates (e.g. filter.py vs filters.py, aggregate.py vs aggregator.py).
- Do not delete tests/ or test_*.py files.
- Do not rewrite files that already satisfy their checklist item.
- Prefer small seed lists (≤15) or config over large hardcoded tables of hosts/APIs/URLs.
- Do not invent long lists of domains or endpoints — a handful of well-known real examples is enough.
- No repetitive filler keys (same name prefix with varied fake TLDs).{test_rules}
{}

Output ONLY:
@@@ FILE: relative/path.ext
<full contents>
"#,
        critique.gaps,
        critique.reason,
        if small_or_heal {
            let help_cmd = cli_help_instruction(cli_mod);
            format!(
                "\n- {help_cmd} after your edit (lazy-import optional heavy deps — not at import time for -h).\n\
- CLI subcommand/flag names in the request (backticks / --flags) must appear VERBATIM in -h — do not rename synonyms.\n\
- On ModuleNotFoundError: create the missing file OR remove the bad import — do not oscillate.\n\
- Declare third-party packages in pyproject.toml/requirements.txt; prefer stdlib when enough.\n\
- No empty stub modules for files the checklist/checkpoint requires."
            )
        } else {
            String::new()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_done() {
        let c = parse_critique("@@@ STATUS: DONE\n@@@ REASON: good enough\n");
        assert!(c.done);
    }

    #[test]
    fn mvp_detects_cli() {
        let mut m = BTreeMap::new();
        m.insert(
            "scripts/job_search/cli.py".into(),
            "import argparse\ndef main():\n    p=argparse.ArgumentParser()\n    p.parse_args()\n"
                .into(),
        );
        m.insert("scripts/requirements.txt".into(), "requests>=2\n".into());
        assert!(looks_like_mvp(&m));
    }

    #[test]
    fn sanitize_strips_tools() {
        let g = sanitize_goal("build a cli\n### Workspace Tools\nfoo\ngit_status:\nx");
        assert!(g.contains("build a cli"));
        assert!(!g.contains("git_status"));
    }

    #[test]
    fn protects_test_paths() {
        assert!(is_protected_test_path("test_solution.py"));
        assert!(is_protected_test_path("foo/test_foo.py"));
        assert!(!is_protected_test_path("solution.py"));
    }

    #[test]
    fn detects_python_harness() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("test_solution.py"), "import unittest\n").unwrap();
        fs::write(dir.path().join("solution.py"), "pass\n").unwrap();
        assert_eq!(detect_test_harness(dir.path()), TestHarness::PythonUnittest);
    }

    #[test]
    fn snapshot_collects_tests_subdir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(
            dir.path().join("tests/test_filters.py"),
            "import unittest\nclass T(unittest.TestCase):\n    def test_ok(self):\n        self.assertTrue(True)\n",
        )
        .unwrap();
        let snap = collect_test_snapshot(dir.path());
        assert!(snap.contains_key("tests/test_filters.py"));
        assert!(snap["tests/test_filters.py"].contains("test_ok"));
    }

    #[test]
    fn restore_snapshot_rewrites_deleted_tests() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        let body = "import unittest\nclass T(unittest.TestCase):\n    def test_a(self):\n        self.assertEqual(1, 1)\n";
        fs::write(dir.path().join("tests/test_filters.py"), body).unwrap();
        let snap = collect_test_snapshot(dir.path());
        fs::remove_file(dir.path().join("tests/test_filters.py")).unwrap();
        ensure_origin_test_snapshot(dir.path(), &snap);
        let restored = fs::read_to_string(dir.path().join("tests/test_filters.py")).unwrap();
        assert!(restored.contains("test_a"));
    }

    #[test]
    fn goal_requires_tests_detects_keywords() {
        assert!(goal_requires_tests("add tests under tests/"));
        assert!(goal_requires_tests("make unittest discover pass"));
        assert!(!goal_requires_tests("add a CLI with argparse"));
    }

    #[test]
    fn unittest_meaningful_pass_allows_mixed_skip() {
        let out = "\
test_a ... skipped 'later'\n\
test_b ... ok\n\
\n\
----------------------------------------------------------------------\n\
Ran 2 tests in 0.001s\n\
\n\
OK (skipped=1)\n";
        assert!(unittest_meaningful_pass(out));
    }

    #[test]
    fn unittest_meaningful_pass_rejects_all_skipped() {
        let out = "\
test_a ... skipped 'x'\n\
\n\
----------------------------------------------------------------------\n\
Ran 1 test in 0.001s\n\
\n\
OK (skipped=1)\n";
        assert!(!unittest_meaningful_pass(out));
    }

    #[test]
    fn checkpoint_heal_goal_detected() {
        assert!(goal_is_checkpoint_heal(
            "Checkpoint `scaffold` FAILED. Fix it."
        ));
        assert!(!goal_is_checkpoint_heal("build a CLI"));
    }

    #[test]
    fn infer_small_model_from_tags() {
        use crate::config::infer_small_model;
        assert!(infer_small_model("Qwen2.5-Coder:7b-instruct-q4_K_M"));
        assert!(infer_small_model("qwen3:4b-instruct"));
        assert!(infer_small_model("ornith-1.0-9B"));
        assert!(!infer_small_model("qwen3-coder-next:q4_K_M"));
        assert!(!infer_small_model("Qwen2.5-Coder:32B"));
    }

    #[test]
    fn agent_profile_small_override() {
        let mut cfg = crate::config::VibeConfig::default();
        cfg.reason_model = "qwen3-coder-next:q4_K_M".into();
        cfg.agent_profile = "small".into();
        assert!(cfg.is_small_agent());
        cfg.agent_profile = "default".into();
        assert!(!cfg.is_small_agent());
        cfg.agent_profile = "auto".into();
        cfg.reason_model = "Qwen2.5-Coder:7b".into();
        assert!(cfg.is_small_agent());
    }

    #[test]
    fn extract_cli_tokens_from_verify_prompt() {
        let goal = "CLI `verify` subcommand: read results.jsonl, check each job.";
        let toks = extract_cli_help_tokens(goal);
        assert!(
            toks.iter().any(|t| t.eq_ignore_ascii_case("verify")),
            "got {toks:?}"
        );
    }

    #[test]
    fn extract_cli_tokens_from_help_contains_fail() {
        let goal = "Checkpoint `opencheck` FAILED.\nFAIL -h contains 'verify' (exit=0)\nhelp_contains:verify\n";
        let toks = extract_cli_help_tokens(goal);
        assert!(
            toks.iter().any(|t| t.eq_ignore_ascii_case("verify")),
            "got {toks:?}"
        );
    }

    #[test]
    fn extract_cli_tokens_skips_paths() {
        let goal = "Add `jobs_aggregator/opencheck.py`: check_job_open.";
        let toks = extract_cli_help_tokens(goal);
        assert!(
            !toks
                .iter()
                .any(|t| t.contains("opencheck") || t.contains("jobs")),
            "got {toks:?}"
        );
    }

    #[test]
    fn help_token_gate_detects_missing_verify() {
        let help = "usage: jobs_aggregator [-h] {opencheck} ...\n  opencheck  Run opencheck\n";
        let tokens: Vec<String> = vec!["verify".into()];
        let missing: Vec<&String> = tokens
            .iter()
            .filter(|t| !help.to_ascii_lowercase().contains(&t.to_ascii_lowercase()))
            .collect();
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn normalize_heal_plan_keeps_fix_section() {
        let raw =
            "@@@ DIAGNOSIS\n- missing rate in -h\n\n@@@ FIX PLAN\n- cli.py — add --rate-min\n";
        let p = normalize_heal_plan(raw);
        assert!(p.contains("FIX PLAN"));
        assert!(p.contains("--rate-min"));
        assert!(p.contains("DIAGNOSIS"));
    }

    #[test]
    fn fail_looks_like_cli_gate_detects_entrypoint() {
        assert!(fail_looks_like_cli_gate(
            "CLI entrypoint / help-text gate failed\nNo module named foo.__main__"
        ));
        assert!(!fail_looks_like_cli_gate(
            "AssertionError: expected 3 got 2"
        ));
    }

    #[test]
    fn extract_cli_tokens_skips_pip_install() {
        let goal =
            "Doctor: run `pip install -r requirements.txt` then `python -m unittest discover`.";
        let toks = extract_cli_help_tokens(goal);
        assert!(
            !toks.iter().any(|t| matches!(
                t.to_ascii_lowercase().as_str(),
                "pip" | "install" | "unittest" | "discover" | "python"
            )),
            "got {toks:?}"
        );
    }

    #[test]
    fn sticky_harness_when_snapshot_nonempty() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(
            dir.path().join("tests/test_filters.py"),
            "import unittest\nclass T(unittest.TestCase):\n    def test_ok(self):\n        self.assertTrue(True)\n",
        )
        .unwrap();
        let snap = collect_test_snapshot(dir.path());
        fs::remove_file(dir.path().join("tests/test_filters.py")).unwrap();
        // Disk no longer has tests, but snapshot should still imply PythonUnittest sticky.
        assert_eq!(detect_test_harness(dir.path()), TestHarness::None);
        assert!(!snap.is_empty());
    }

    #[test]
    fn literal_sprawl_rejects_exploded_host_dict() {
        let mut body = String::from("KNOWN = {\n");
        for tld in [
            "com", "net", "org", "io", "co", "dev", "eu", "uk", "info", "com",
        ] {
            for i in 0..8 {
                body.push_str(&format!("    'djangojobs{i}.{tld}': 1.3,\n"));
            }
        }
        body.push_str("}\n");
        let smell = literal_sprawl_smell("jobs_aggregator/link_quality.py", &body);
        assert!(smell.is_some(), "expected smell on exploded host dict");
        let msg = smell.unwrap();
        assert!(
            msg.contains("dict entries") || msg.contains("domain") || msg.contains("prefix"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn literal_sprawl_allows_small_host_allowlist() {
        let body = r#"
KNOWN = {
    'linkedin.com': 1.5,
    'indeed.com': 1.3,
    'glassdoor.com': 1.2,
    'remote.co': 1.3,
    'remotive.com': 1.3,
    'dice.com': 1.2,
    'stackoverflow.com': 1.3,
    'angel.co': 1.2,
    'flexjobs.com': 1.2,
    'weworkremotely.com': 1.2,
}
"#;
        assert!(literal_sprawl_smell("link_quality.py", body).is_none());
    }

    #[test]
    fn literal_sprawl_allows_normal_code() {
        let body = r#"
def score_url(url: str) -> float:
    if url.startswith("https://"):
        return 1.0
    return 0.5
"#;
        assert!(literal_sprawl_smell("cli.py", body).is_none());
    }

    #[test]
    fn distill_keeps_assertion() {
        let raw = "ok\n\nFAIL: test_all\n----------------------------------------------------------------------\nAssertionError: 5 != 0\n";
        let d = distill_test_failure(raw, 200);
        assert!(d.contains("AssertionError"));
        assert!(d.contains("5 != 0"));
    }

    #[test]
    fn extracts_required_exports() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("test_solution.py"),
            "from solution import two_sum, max_profit\n",
        )
        .unwrap();
        let names = extract_required_python_exports(dir.path());
        assert_eq!(names, vec!["max_profit".to_string(), "two_sum".to_string()]);
    }

    #[test]
    fn validates_exports_present() {
        let body = "def two_sum(nums, target):\n    return [0, 1]\n";
        assert!(validate_python_exports(body, &["two_sum".into()]).is_ok());
        assert!(validate_python_exports(body, &["two_sum".into(), "merge".into()]).is_err());
    }

    #[test]
    fn validates_solution_class() {
        let body = "class Solution:\n    def foo(self):\n        return 1\n";
        assert!(validate_python_exports(body, &["Solution".into()]).is_ok());
    }

    #[test]
    fn classifies_wrong_answer() {
        let t = "AssertionError: got 0 expected 6\nFAILED (failures=1)\n";
        assert_eq!(classify_test_failure(t), FailureClass::WrongAnswer);
    }

    #[test]
    fn classifies_runtime_empty_min() {
        let t = "ValueError: min() iterable argument is empty\n";
        assert_eq!(classify_test_failure(t), FailureClass::RuntimeError);
    }

    #[test]
    fn classifies_name_error() {
        let t = "NameError: name 'Counter' is not defined\n";
        assert_eq!(classify_test_failure(t), FailureClass::NameOrImport);
    }

    #[test]
    fn extracts_call_and_expected() {
        let t = "_got = candidate(s = \"ab\", k = 1)\n_exp = True\nAssertionError: got False expected True\n";
        assert_eq!(
            extract_candidate_call_args(t).as_deref(),
            Some("s = \"ab\", k = 1")
        );
        assert_eq!(extract_expected_value(t).as_deref(), Some("True"));
    }

    #[test]
    fn extracts_call_from_assertion_message() {
        let t = r#"AssertionError: candidate(s = "pqrstuvwabcdef") got 0 expected -inf"#;
        assert_eq!(
            extract_candidate_call_args(t).as_deref(),
            Some("s = \"pqrstuvwabcdef\"")
        );
        assert_eq!(extract_expected_value(t).as_deref(), Some("float('-inf')"));
    }

    #[test]
    fn extracts_numeric_expected() {
        let t = "AssertionError: got 0 expected 6\n";
        assert_eq!(extract_expected_value(t).as_deref(), Some("6"));
    }

    #[test]
    fn binder_skips_def_check_param() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"
def check(candidate):
    _got = candidate(s = "a")
    _exp = 1

class TestLCD(unittest.TestCase):
    def test_all(self):
        check(Solution().maxDifference)
"#;
        fs::write(dir.path().join("test_solution.py"), body).unwrap();
        assert_eq!(
            read_candidate_binder(dir.path()).as_deref(),
            Some("Solution().maxDifference")
        );
    }

    #[test]
    fn static_hints_flag_unguarded_min_and_inf() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"
from collections import Counter
class Solution:
    def maxDifference(self, s: str) -> int:
        c = Counter(s)
        odds = [v for v in c.values() if v % 2]
        evens = [v for v in c.values() if v % 2 == 0]
        return max(odds) - min(evens)
"#;
        fs::write(dir.path().join("solution.py"), body).unwrap();
        let probe = "PROBE_CALL candidate(s = \"abc\")\nPROBE_EXPECTED -inf\nPROBE_EXC ValueError: min() iterable argument is empty\n";
        let hints = build_static_fix_hints(
            dir.path(),
            "solution.py",
            FailureClass::RuntimeError,
            "ValueError: min() iterable argument is empty\n",
            Some(probe),
        )
        .expect("hints");
        assert!(hints.contains("calls_min"), "{hints}");
        assert!(
            hints.contains("min_max_unguarded") || hints.contains("empty"),
            "{hints}"
        );
        assert!(hints.contains("FIX:"), "{hints}");
        assert!(
            hints.to_lowercase().contains("inf") || hints.contains("empty"),
            "{hints}"
        );
    }

    #[test]
    fn static_hints_flag_star_without_split() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"
class Solution:
    def hasMatch(self, s: str, p: str) -> bool:
        return p in s
"#;
        fs::write(dir.path().join("solution.py"), body).unwrap();
        let probe = "PROBE_CALL candidate(s = \"abcdefg\",p = \"a*d*g\")\nPROBE_EXPECTED True\nPROBE_GOT False\n";
        let hints = build_static_fix_hints(
            dir.path(),
            "solution.py",
            FailureClass::WrongAnswer,
            r#"AssertionError: candidate(s = "abcdefg",p = "a*d*g") got False expected True"#,
            Some(probe),
        )
        .expect("hints");
        assert!(
            hints.contains("split/partition") || hints.contains("'*'"),
            "{hints}"
        );
    }

    #[test]
    fn analyze_python_solution_reads_flags() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"
class Solution:
    def hasMatch(self, s, p):
        a, b = p.split("*")
        return not (a in s)
"#;
        fs::write(dir.path().join("solution.py"), body).unwrap();
        let f = analyze_python_solution(&dir.path().join("solution.py")).expect("facts");
        assert!(f.splits_on_star);
        assert!(f.returns_not);
        assert!(f.methods.iter().any(|m| m == "hasMatch"));
    }

    #[test]
    fn scrub_strips_think_and_keeps_advice() {
        let raw = "<thinking>\nlong secret chain\n</thinking>\nROOT CAUSE: empty min()\nCONCRETE FIX:\n- return -inf\n@@@ FILE: solution.py\nbad";
        let out = scrub_reasoning_output(raw);
        assert!(out.contains("ROOT CAUSE"));
        assert!(out.contains("empty min"));
        assert!(!out.contains("secret chain"));
        assert!(!out.contains("@@@ FILE"));
    }

    #[test]
    fn repair_prompt_includes_debug_advice() {
        let p = build_repair_prompt(
            "goal",
            "stub",
            "AssertionError: got 0 expected 6",
            "solution.py",
            &[],
            false,
            Some("PROBE_GOT 0\nPROBE_EXPECTED 6"),
            Some("FACTS:\n- calls_min\n"),
            Some("ROOT CAUSE: off-by-one\nCONCRETE FIX:\n- fix loop bound"),
        );
        assert!(p.contains("Debug-model diagnosis"));
        assert!(p.contains("off-by-one"));
        assert!(p.contains("Static diagnosis"));
        assert!(p.contains("PROBE_GOT 0"));
    }
}

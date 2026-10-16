//! Fixture eval suite for shadow heal + dual-model smoke.
//!
//! Usage:
//!   cargo run -p vibe_core --bin vibe_eval
//!   cargo run -p vibe_core --bin vibe_eval -- --smoke-only

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use vibe_core::config::VibeConfig;
use vibe_core::ollama::OllamaClient;
use vibe_core::shadow::{build_edit_prompt, ShadowWorkspace};

struct Case {
    name: &'static str,
    dir: &'static str,
    prompt: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "wrong_return_type",
        dir: "rust_broken",
        prompt: "Fix compile errors. Keep greet(). Make add return i32 as the sum of a and b.",
    },
    Case {
        name: "missing_import",
        dir: "rust_missing_import",
        prompt: "Fix compile errors. Add any missing imports. Keep counts() behavior.",
    },
];

#[tokio::main]
async fn main() -> ExitCode {
    let smoke_only = env::args().any(|a| a == "--smoke-only");
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
        eprintln!("start ollama and pull models (see README)");
        return ExitCode::FAILURE;
    }

    let reason = ollama.resolve_reason_model(&cfg).await;
    println!("ghost={}", cfg.ghost_model);
    println!("reason={reason}");

    println!("warming dual models…");
    match ollama.warm_dual(&cfg.ghost_model, &reason).await {
        Ok(()) => println!("smoke: dual generate OK"),
        Err(e) => {
            eprintln!("smoke warm failed (is ghost model pulled?): {e}");
            if smoke_only {
                return ExitCode::FAILURE;
            }
            eprintln!("continuing heal eval anyway…");
        }
    }

    if smoke_only {
        return ExitCode::SUCCESS;
    }

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures_root = manifest.join("../../fixtures");
    let mut failed = 0usize;

    for case in CASES {
        print!("case {} … ", case.name);
        let fixture = fixtures_root.join(case.dir);
        if !fixture.exists() {
            println!("SKIP (missing fixture)");
            continue;
        }
        match run_case(&fixture, case.prompt, &ollama, &cfg, &reason).await {
            Ok(attempts) => println!("PASS (attempts={attempts})"),
            Err(e) => {
                println!("FAIL: {e}");
                failed += 1;
            }
        }
    }

    if failed > 0 {
        eprintln!("{failed} case(s) failed");
        ExitCode::FAILURE
    } else {
        println!("all heal cases passed");
        ExitCode::SUCCESS
    }
}

async fn run_case(
    fixture: &Path,
    user_prompt: &str,
    ollama: &OllamaClient,
    cfg: &VibeConfig,
    reason: &str,
) -> Result<u32, String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    copy_dir(fixture, tmp.path()).map_err(|e| e.to_string())?;
    let root = tmp.path().to_path_buf();

    let original = fs::read_to_string(root.join("src/lib.rs")).map_err(|e| e.to_string())?;
    let shadow = ShadowWorkspace::new(&root).map_err(|e| e.to_string())?;
    shadow.sync_from_origin().map_err(|e| e.to_string())?;
    shadow
        .stage_file_change("src/lib.rs", &original)
        .map_err(|e| e.to_string())?;
    if shadow.verify_build().is_ok() {
        return Err("fixture unexpectedly already compiles".into());
    }

    let prompt = build_edit_prompt("src/lib.rs", &original, None, user_prompt, "(no context)");
    let heal = shadow
        .auto_heal_loop("src/lib.rs", &prompt, ollama, cfg, reason)
        .await
        .map_err(|e| e.to_string())?;
    if !heal.success {
        return Err(heal.diagnostics.unwrap_or_else(|| "heal failed".into()));
    }
    shadow
        .apply_to_origin("src/lib.rs", &heal.content)
        .map_err(|e| e.to_string())?;
    Ok(heal.attempts)
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

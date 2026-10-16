# Security Policy

## Supported versions

This project is **experimental (`0.x`)**. Only the latest `main` branch receives fixes.

## What to expect

Maqqa runs **local** agent loops that can:

- Read and write files in an opened workspace
- Execute toolchain commands for verify/heal (e.g. `python3`, `cargo check`, unittest)
- Call a configured Ollama HTTP endpoint

Treat an opened folder like giving a coding agent access to that tree. Prefer disposable or version-controlled workspaces.

## Reporting a vulnerability

Please **do not** open a public issue for security-sensitive reports.

Email or message the maintainers privately with:

- Impact (RCE, path escape, secret exfil, etc.)
- Steps to reproduce
- Affected commit / tag if known

We will acknowledge and aim to patch before any public disclosure.

## Hardening tips

- Point `ollama_url` only at a trusted host (local or your own tunnel).
- Do not commit `~/.vibe/config.toml`, `.env`, or workspace `.vibe/` logs.
- Review agent diffs before Accept when working on sensitive repos.

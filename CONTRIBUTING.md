# Contributing

Thanks for taking an interest in Maqqa. The project is early; small, focused changes are easiest to review.

## Prerequisites

- Rust stable (edition 2021)
- Node 20+ (desktop UI)
- [Ollama](https://ollama.com) for agent / smoke tests that call a model

## Setup

```bash
cp config.example.toml ~/.vibe/config.toml   # then edit model tags
cargo test -p vibe_core --lib
cargo test -p vibe_ipc
cd apps/desktop && npm install
```

Optional smoke (needs Ollama):

```bash
cargo run -p vibe_core --bin vibe_eval -- --smoke-only
```

## Development norms

- Prefer fixing / extending `crates/vibe_core` with unit tests when behavior changes.
- Do not commit `artifacts/`, demo videos, `.vibe/`, or local `~/.vibe` configs.
- Keep PRs scoped; mention scenario/bench impact if you touch the agent loop.
- Run before opening a PR:

```bash
cargo fmt --all -- --check
cargo clippy -p vibe_core -p vibe_ipc --all-targets -- -D warnings
cargo test -p vibe_core --lib
cargo test -p vibe_ipc
```

Desktop TypeScript:

```bash
cd apps/desktop && npm run build
```

## Scenario / bench runs

Long `vibe_scenario` / `vibe_bench` jobs are optional for PRs unless your change targets agent quality. Prefer a short slice (`--from 1 --to 10`) when demonstrating a fix.

## License

By contributing, you agree that your contributions are licensed under the MIT License.

# Scenario: remote B2B contractor job aggregator

End-to-end **prompt-driven** build of a Python tool that aggregates remote contractor jobs,
filters them (remote, languages, contract/perm, rates), summarizes listings, and verifies
links are still open.

The product is **not** checked into `workspace/` — the vibe agent must create it by running
the prompts in [`prompts.jsonl`](prompts.jsonl) (~100 steps).

## Audience

Worldwide **remote B2B / corp-to-corp contractors** hunting paid contract work (hourly rates,
language stack filters, open-application checks).

## Run (requires Ollama + reason model)

```bash
# regenerate prompts if needed
python3 scripts/gen_job_scenario_prompts.py

# dry-run: list prompts
cargo run -p vibe_core --bin vibe_scenario -- --list

# run a slice (recommended while iterating)
cargo run -p vibe_core --bin vibe_scenario -- --from 1 --to 10

# full scenario (~100 agent sessions; long-running)
cargo run -p vibe_core --bin vibe_scenario -- --from 1 --to 100 --json /tmp/jobs_scenario.json

# re-check checkpoints against an existing work dir
cargo run -p vibe_core --bin vibe_scenario -- --workspace /path/to/work --check-only
```

## Checkpoints

Defined in [`checkpoints.json`](checkpoints.json), executed by
[`checkpoints/run_checks.py`](checkpoints/run_checks.py) when a prompt sets `"checkpoint"`.

| id | After roughly | Expectation |
|----|---------------|-------------|
| scaffold | early | package + `python -m jobs_aggregator -h` |
| filters | mid | remote/lang/type/rate filters + unit tests |
| sources | mid | sources + aggregator + mock offline |
| summary | mid-late | summarize + markdown/json report |
| opencheck | late | verify open links / dead pages |
| e2e | end | pipeline + unittest suite green |

## Rules

- Only the **vibe agent** implements the product (via these prompts).
- Engine fixes are allowed if the agent/harness is broken.
- Do not hand-write `jobs_aggregator/` into `workspace/` for a “pass”.

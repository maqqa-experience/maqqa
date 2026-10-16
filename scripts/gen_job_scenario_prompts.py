#!/usr/bin/env python3
"""Generate fixtures/scenarios/remote_b2b_jobs/prompts.jsonl (~100 agent prompts)."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "fixtures/scenarios/remote_b2b_jobs/prompts.jsonl"

# Each item: (phase, prompt, optional checkpoint id after this step)
STEPS: list[tuple[str, str, str | None]] = []


def add(phase: str, prompt: str, checkpoint: str | None = None) -> None:
    STEPS.append((phase, prompt.strip(), checkpoint))


# —— 1–12 Scaffold ——
add(
    "scaffold",
    """Create a new Python package `jobs_aggregator` for a remote B2B contractor job search tool.
Layout:
- jobs_aggregator/__init__.py
- jobs_aggregator/cli.py (argparse entry)
- jobs_aggregator/models.py (Job dataclass/pydantic-free typed dicts OK)
- pyproject.toml or requirements.txt
- README.md describing the product for worldwide remote B2B contractors
Do not scrape yet — scaffold only. CLI should print --help.""",
)
add(
    "scaffold",
    """Add a console script entry so `python -m jobs_aggregator` and optionally `jobs-agg` run the CLI.
Ensure `python -m jobs_aggregator -h` works from the repo root.""",
)
add(
    "scaffold",
    """In models.py define a Job record with at least: id, title, company, url, source, location,
is_remote, employment_type (contract|permanent|unknown), languages (list[str]),
hourly_rate_min, hourly_rate_max, currency, description, summary, posted_at, last_checked_at,
is_open (bool|None), open_check_notes. Use dataclasses.""",
)
add(
    "scaffold",
    """Add jobs_aggregator/config.py with defaults: user_agent, request timeout, max_results,
default currency USD, supported languages list including php, python, javascript, typescript,
rust, go, java, ruby, csharp.""",
)
add(
    "scaffold",
    """Add jobs_aggregator/storage.py with JSONL save/load for Job lists under .jobs_cache/results.jsonl.
Include atomic write.""",
)
add(
    "scaffold",
    """Add empty package jobs_aggregator/sources/ with __init__.py and a BaseSource ABC:
fetch(self, query: SearchQuery) -> list[Job]. Define SearchQuery in models.py.""",
)
add(
    "scaffold",
    """Define SearchQuery with: keywords, remote_only, languages, employment_types,
hourly_rate_min, hourly_rate_max, currency, countries (optional), limit.""",
)
add(
    "scaffold",
    """Wire CLI subcommand `init` that creates .jobs_cache/ and writes a sample empty results file.
Document it in README.""",
)
add(
    "scaffold",
    """Add a Makefile or just document: pip install -e . and python -m jobs_aggregator -h.
Keep dependencies minimal (requests, beautifulsoup4 optional later).""",
)
add(
    "scaffold",
    """Add jobs_aggregator/logging_utils.py with structured stderr logging (level via --verbose).
CLI --verbose flag.""",
    "scaffold",
)

# —— 13–28 Filters / CLI ——
add(
    "filters",
    """Extend CLI with `search` subcommand accepting:
--keywords, --remote / --remote-only, --lang (repeatable), --type contract|perm|any,
--rate-min, --rate-max, --currency, --limit. Parse into SearchQuery.""",
)
add(
    "filters",
    """Normalize employment type aliases: contract, contractor, freelance, b2b -> contract;
perm, permanent, full-time, fte -> permanent.""",
)
add(
    "filters",
    """Add language alias map: py->python, js->javascript, ts->typescript, php->php, golang->go, c#->csharp.
CLI --lang should accept aliases.""",
)
add(
    "filters",
    """Implement jobs_aggregator/filters.py with apply_filters(jobs, query) enforcing remote_only,
languages intersection, employment_types, and hourly rate window (skip rate filter when job rates unknown
unless --require-rate is set).""",
)
add(
    "filters",
    """Add CLI --require-rate that drops jobs without hourly_rate_min/max when filtering.""",
)
add(
    "filters",
    """Add --include-unknown-remote flag (default off when --remote-only): if off, drop jobs where
is_remote is None under remote_only mode.""",
)
add(
    "filters",
    """Add unit tests in tests/test_filters.py covering remote, language, contract/perm, and rate filters
with synthetic Job fixtures. Use unittest or pytest.""",
)
add(
    "filters",
    """Make tests pass: python -m unittest discover -s tests -v (or pytest). Fix any import path issues.""",
)
add(
    "filters",
    """CLI `search` should accept --json and --markdown output formats (stub: print filtered count for now
if no sources yet).""",
)
add(
    "filters",
    """Add jobs_aggregator/query_parse.py to parse a free-text string like
'remote python contract $80-$120' into SearchQuery fields. Add CLI --query for that shorthand.""",
    "filters",
)

# —— 29–48 Sources ——
add(
    "sources",
    """Implement sources/remoteok.py fetching https://remoteok.com/api (JSON). Map to Job with source=remoteok.
Respect User-Agent. Handle HTTP errors gracefully.""",
)
add(
    "sources",
    """Tag RemoteOK jobs: set is_remote=True; detect languages from tags/description; employment_type
heuristic from text (contract/freelance vs full-time).""",
)
add(
    "sources",
    """Implement sources/remotive.py using Remotive public API if available, else document stub that returns [].
Prefer real HTTP when the API is reachable.""",
)
add(
    "sources",
    """Implement sources/arbeitnow.py or a second free board API. If blocked, implement a clearly marked
MockSource that loads fixtures/scenarios sample JSON for offline demos.""",
)
add(
    "sources",
    """Add sources/registry.py listing enabled sources. CLI --source repeatable filter (default: all enabled).""",
)
add(
    "sources",
    """Implement aggregator.py: run selected sources concurrently (ThreadPoolExecutor), merge Job lists,
dedupe by normalized url and by (company,title) fingerprint.""",
)
add(
    "sources",
    """Add URL normalization: strip tracking query params (utm_*, fbclid), lowercase host, remove trailing slash.""",
)
add(
    "sources",
    """Wire CLI `search` to actually call aggregator + filters + storage save. Print summary count per source.""",
)
add(
    "sources",
    """Add fixtures/sample_jobs.json with 8 synthetic jobs spanning remote/on-site, py/php, contract/perm,
with/without rates — used by MockSource and tests.""",
)
add(
    "sources",
    """Add MockSource reading fixtures/sample_jobs.json. Enable with --source mock for deterministic runs.""",
)
add(
    "sources",
    """Add tests/test_dedupe.py ensuring duplicate urls collapse and fingerprint dedupe works.""",
)
add(
    "sources",
    """Add polite rate limiting: sources/http_util.py with session, timeout, retries (3), backoff.""",
)
add(
    "sources",
    """Respect robots/terms: add README ethics section — only public APIs / permitted boards; no authenticated scrape.
Add --offline that forces mock source only.""",
)
add(
    "sources",
    """CLI `sources` subcommand lists registered sources and whether they are online/offline capable.""",
)
add(
    "sources",
    """Catch and log per-source failures without aborting the whole search.""",
    "sources",
)

# —— 49–65 Summarization ——
add(
    "summary",
    """Add jobs_aggregator/summarize.py with summarize_job(job, mode=heuristic|llm) -> str.
Heuristic: first 400 chars of description cleaned + title/company one-liner.""",
)
add(
    "summary",
    """Add Ollama-based LLM summarizer: POST to local Ollama /api/generate with a short prompt asking for
3-bullet contractor-focused summary (rate, stack, remote/B2B signals). Env JOBS_OLLAMA_URL / JOBS_OLLAMA_MODEL.
If Ollama down, fall back to heuristic.""",
)
add(
    "summary",
    """CLI --summarize heuristic|llm|off (default heuristic). Store result in Job.summary.""",
)
add(
    "summary",
    """Summaries must include: role one-liner, key skills/languages, compensation if present, application link domain.""",
)
add(
    "summary",
    """Add batch summarize for a results file: CLI `summarize --input .jobs_cache/results.jsonl --mode heuristic`.""",
)
add(
    "summary",
    """tests/test_summarize.py: heuristic summary non-empty for sample jobs; llm path mocked/skipped if no ollama.""",
)
add(
    "summary",
    """Markdown report writer: jobs_aggregator/report.py render_markdown(jobs) with title, company, summary, url,
employment_type, languages, rate, is_open.""",
)
add(
    "summary",
    """CLI `search --format markdown` writes report to stdout or --out report.md.""",
)
add(
    "summary",
    """JSON output: --format json emits array of Job as dicts (include summary and url).""",
)
add(
    "summary",
    """Add --sort rate_desc|posted|title for report ordering.""",
    "summary",
)

# —— 66–85 Open / freshness checks ——
add(
    "opencheck",
    """Add jobs_aggregator/opencheck.py: check_job_open(job) -> (is_open, notes).
HTTP GET/HEAD url; treat 404/410/gone as closed; 200 + closed phrases as closed; timeouts as unknown.""",
)
add(
    "opencheck",
    """Closed phrase heuristics (case-insensitive): 'no longer accepting', 'position filled', 'job closed',
'applications closed', 'this job is no longer available', 'page not found'.""",
)
add(
    "opencheck",
    """Follow redirects; if final URL looks like a login wall or homepage fallback, mark unknown with notes.""",
)
add(
    "opencheck",
    """CLI `verify` subcommand: read results.jsonl, check each job, update is_open + last_checked_at + notes, rewrite file.""",
)
add(
    "opencheck",
    """CLI search --verify-open runs open-check before output and drops closed jobs (keep unknowns unless --strict-open).""",
)
add(
    "opencheck",
    """Add --strict-open to exclude is_open is None as well.""",
)
add(
    "opencheck",
    """Concurrency: verify with ThreadPoolExecutor max_workers=8 and per-host polite delay.""",
)
add(
    "opencheck",
    """tests/test_opencheck.py: phrase detection unit tests without network; optional network test marked skip.""",
)
add(
    "opencheck",
    """Dead link handling: connection errors -> is_open=None, notes='unreachable'. Do not crash the batch.""",
)
add(
    "opencheck",
    """Add jobs_aggregator/link_quality.py: score urls (https preferred, known job-board hosts boost).
Filter --min-link-score optional.""",
)
add(
    "opencheck",
    """Report section 'Closed / skipped' listing urls dropped by verify.""",
)
add(
    "opencheck",
    """Cache open-check results by url for 24h in .jobs_cache/open_checks.json to avoid re-hitting boards.""",
    "opencheck",
)

# —— 86–100 Product polish / E2E ——
add(
    "polish",
    """End-to-end happy path documented in README:
jobs-agg search --offline --source mock --remote-only --lang python --type contract --rate-min 60 --summarize heuristic --verify-open --format markdown --out /tmp/jobs.md""",
)
add(
    "polish",
    """Implement that offline mock path fully so the command above works without network.""",
)
add(
    "polish",
    """Add jobs_aggregator/pipeline.py run_search(query, options) used by CLI to keep cli.py thin.""",
)
add(
    "polish",
    """Add config file support: .jobs_aggregator.toml or yaml for default langs, rate floor, sources.
CLI flags override file.""",
)
add(
    "polish",
    """B2B focus: add filter --b2b-only heuristic (description mentions b2b, limited company, invoice, corp-to-corp, outside ir35).""",
)
add(
    "polish",
    """Add --timezone or --region free-text soft filter matching location/description (not hard country ISO yet).""",
)
add(
    "polish",
    """Export CSV via --format csv with columns title,company,url,languages,type,rate_min,rate_max,is_open,summary.""",
)
add(
    "polish",
    """Add CONTRIBUTING.md or README section: how to add a new Source subclass.""",
)
add(
    "polish",
    """Tighten error messages: if no jobs match, print which filters removed how many (funnel stats).""",
)
add(
    "polish",
    """Add tests/test_pipeline_offline.py running mock+filters+heuristic summarize+markdown without network.""",
)
add(
    "polish",
    """Ensure python -m unittest discover -s tests -v passes.""",
)
add(
    "polish",
    """Add examples/example_queries.md with 5 example contractor queries (PHP remote B2B, Python $100+/hr, etc.).""",
)
add(
    "polish",
    """Security: sanitize filenames for --out; never execute job description HTML; strip scripts in HTML-to-text.""",
)
add(
    "polish",
    """HTML-to-text helper for descriptions using html.parser or bleach-free stdlib.""",
)
add(
    "polish",
    """Final README rewrite: audience = worldwide remote B2B contractors; feature list (filters, summarize, open-check);
ethics; offline demo command; limitations (boards change, LLM optional).""",
    "e2e",
)

# Focused follow-ups + late polish (avoid empty "hardening" spam)
extra = [
    (
        "filters",
        """Add --exclude-lang repeatable to exclude languages (e.g. exclude java). Cover in tests.""",
    ),
    (
        "filters",
        """Support daily rate conversion heuristic: if description has £500/day convert approx to hourly (/8) for filtering when --assume-day-rate.""",
    ),
    (
        "sources",
        """Add source health ping: HEAD/GET homepage; `jobs-agg sources --ping` shows ok/fail.""",
    ),
    (
        "sources",
        """Persist last successful fetch timestamp per source in .jobs_cache/source_meta.json.""",
    ),
    (
        "summary",
        """When summarizing, detect and highlight 'equity only' or 'unpaid' as warnings in summary.""",
    ),
    (
        "summary",
        """Truncate LLM prompt description to 2k chars to control latency.""",
    ),
    (
        "opencheck",
        """Add dry-run verify --limit 5 for quick smoke.""",
    ),
    (
        "opencheck",
        """Treat HTTP 401/403 on application pages as unknown (not closed) with notes='auth_wall'.""",
    ),
    (
        "polish",
        """Add --max-age-days filter using posted_at when parseable.""",
    ),
    (
        "polish",
        """Ship a tiny demo shell script scripts/demo_offline.sh running the mock search pipeline.""",
    ),
    (
        "polish",
        """Fix any remaining import errors; ensure package is importable as jobs_aggregator.""",
    ),
    (
        "polish",
        """Add py.typed marker or leave note — keep typing consistent across modules.""",
    ),
    (
        "e2e",
        """Run offline demo end-to-end and write artifacts under .jobs_cache/demo_report.md. Confirm at least 1 open mock job survives filters for python+contract+remote.""",
        "e2e",
    ),
]

for item in extra:
    if len(item) == 3:
        add(item[0], item[1], item[2])
    else:
        add(item[0], item[1])

late = [
    (
        "polish",
        """Reject invalid CLI combos: --rate-min > --rate-max must exit non-zero with a clear message.
Add a tiny test or assert in CLI parse.""",
    ),
    (
        "polish",
        """Empty results path: when filters remove everything, print funnel stats and exit 0 (not a crash).""",
    ),
    (
        "polish",
        """Add jobs_aggregator/__main__.py so `python -m jobs_aggregator` is reliable.""",
    ),
    (
        "sources",
        """Document rate limits and caching behavior for each live source in README.""",
    ),
    (
        "summary",
        """Ensure JSON output is valid UTF-8 and does not embed raw HTML from descriptions.""",
    ),
    (
        "opencheck",
        """Add verify --only-unknown to recheck jobs previously marked is_open=None.""",
    ),
    (
        "filters",
        """Support --type b2b as alias for contract + b2b heuristic combined.""",
    ),
    (
        "polish",
        """Add a doctor subcommand: checks python version, optional ollama reachability, cache dir writable.""",
    ),
    (
        "polish",
        """Pin minimal deps in requirements.txt with comments why each exists.""",
    ),
    (
        "polish",
        """Add .gitignore covering .jobs_cache/, __pycache__/, .venv/, report artifacts.""",
    ),
    (
        "e2e",
        """Final acceptance: offline mock search for remote python contract with rate-min 50 produces markdown
containing at least one http link and a non-empty summary field. Save under .jobs_cache/FINAL_DEMO.md.""",
        "e2e",
    ),
    (
        "polish",
        """Sweep for leftover NotImplementedError / pass stubs in jobs_aggregator public APIs; implement or remove.""",
    ),
    (
        "polish",
        """Ensure all public modules have docstrings describing contractor/B2B purpose in one line.""",
    ),
    (
        "polish",
        """Add CHANGELOG.md with Unreleased section summarizing filters, sources, summarize, opencheck.""",
    ),
    (
        "polish",
        """Version bump to 0.1.0 in pyproject/setup metadata if present.""",
    ),
]
for item in late:
    if len(STEPS) >= 100:
        break
    if len(item) == 3:
        add(item[0], item[1], item[2])
    else:
        add(item[0], item[1])

while len(STEPS) < 100:
    n = len(STEPS) + 1
    add(
        "polish",
        f"""Prompt {n}: improve UX of `jobs-agg search --help` help text for remote B2B contractors
(examples of --lang python --type contract --remote-only --rate-min).""",
    )

STEPS = STEPS[:100]

def main() -> None:
    OUT.parent.mkdir(parents=True, exist_ok=True)
    with OUT.open("w", encoding="utf-8") as f:
        for idx, (phase, prompt, checkpoint) in enumerate(STEPS, start=1):
            row = {
                "id": idx,
                "phase": phase,
                "prompt": prompt,
                "checkpoint": checkpoint,
            }
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
    print(f"wrote {len(STEPS)} prompts -> {OUT}")


if __name__ == "__main__":
    main()

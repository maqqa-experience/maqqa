#!/usr/bin/env python3
"""Import newfacade/LeetCodeDataset into slim JSONL for vibe_bench.

Source (MIT): https://github.com/newfacade/LeetCodeDataset

Usage:
  python3 scripts/import_leetcode.py
  python3 scripts/import_leetcode.py --version v0.3.1 --splits test,train
  python3 scripts/import_leetcode.py --clone   # fetch third_party/LeetCodeDataset if missing
"""

from __future__ import annotations

import argparse
import gzip
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
THIRD = ROOT / "third_party" / "LeetCodeDataset"
OUT = ROOT / "fixtures" / "leetcode"
KEEP = (
    "task_id",
    "question_id",
    "difficulty",
    "tags",
    "problem_description",
    "starter_code",
    "prompt",
    "entry_point",
    "test",
    "query",
)
REPO = "https://github.com/newfacade/LeetCodeDataset.git"


def clone_if_needed() -> None:
    if (THIRD / "data").is_dir():
        return
    THIRD.parent.mkdir(parents=True, exist_ok=True)
    if THIRD.exists():
        shutil.rmtree(THIRD)
    print(f"cloning {REPO} → {THIRD}", file=sys.stderr)
    subprocess.check_call(
        ["git", "-c", "core.hooksPath=/dev/null", "clone", "--depth", "1", REPO, str(THIRD)]
    )


def slim_file(src: Path, dst: Path) -> int:
    n = 0
    with gzip.open(src, "rt", encoding="utf-8") as fin, gzip.open(
        dst, "wt", encoding="utf-8"
    ) as fout:
        for line in fin:
            row = json.loads(line)
            slim = {k: row[k] for k in KEEP if k in row}
            if not slim.get("task_id") or not slim.get("test") or not slim.get("starter_code"):
                continue
            # Never ship reference solutions into the bench corpus.
            fout.write(json.dumps(slim, ensure_ascii=False) + "\n")
            n += 1
    return n


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--version", default="v0.3.1")
    ap.add_argument("--splits", default="test,train", help="comma list: test,train")
    ap.add_argument("--clone", action="store_true", help="clone upstream if missing")
    ap.add_argument(
        "--source-dir",
        type=Path,
        default=None,
        help="override path to LeetCodeDataset checkout",
    )
    args = ap.parse_args()

    source = args.source_dir or THIRD
    if args.clone or not (source / "data").is_dir():
        if args.source_dir:
            print(f"missing data under {source}", file=sys.stderr)
            return 1
        clone_if_needed()
        source = THIRD

    OUT.mkdir(parents=True, exist_ok=True)
    license_src = source / "LICENSE"
    if license_src.exists():
        shutil.copy2(license_src, OUT / "LICENSE")

    counts = {}
    for split in [s.strip() for s in args.splits.split(",") if s.strip()]:
        name = f"LeetCodeDataset-{args.version}-{split}.jsonl.gz"
        src = source / "data" / name
        if not src.exists():
            print(f"missing {src}", file=sys.stderr)
            return 1
        dst = OUT / f"{args.version}-{split}.jsonl.gz"
        n = slim_file(src, dst)
        counts[split] = n
        print(f"wrote {dst} ({n} problems)")

    meta = {
        "source": "newfacade/LeetCodeDataset",
        "license": "MIT",
        "url": REPO,
        "version": args.version,
        "fields": list(KEEP),
        "counts": counts,
        "note": "Slim eval corpus for vibe_bench. Reference solutions intentionally omitted.",
    }
    (OUT / "manifest.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(json.dumps(meta, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

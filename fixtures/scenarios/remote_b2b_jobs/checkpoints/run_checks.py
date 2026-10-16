#!/usr/bin/env python3
"""Validate workspace against a named checkpoint from checkpoints.json."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--workspace", type=Path, required=True)
    ap.add_argument("--checkpoint", required=True)
    ap.add_argument(
        "--spec",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "checkpoints.json",
    )
    args = ap.parse_args()
    spec = json.loads(args.spec.read_text())
    if args.checkpoint not in spec:
        print(f"unknown checkpoint: {args.checkpoint}", file=sys.stderr)
        return 2
    checks = spec[args.checkpoint]["checks"]
    ws: Path = args.workspace
    failed = 0
    py_bin = sys.executable

    def ensure_venv_and_install() -> str:
        """Use a workspace .venv so pip works under PEP 668 Homebrew Python."""
        nonlocal py_bin
        venv = ws / ".venv"
        if sys.platform == "win32":
            candidate = venv / "Scripts" / "python.exe"
        else:
            candidate = venv / "bin" / "python"
        if not candidate.exists():
            proc = subprocess.run(
                [sys.executable, "-m", "venv", str(venv)],
                cwd=ws,
                capture_output=True,
                text=True,
                timeout=120,
            )
            if proc.returncode != 0:
                print(
                    "WARN venv create failed:\n"
                    + (proc.stderr or "")[-400:],
                    file=sys.stderr,
                )
                return sys.executable
        py_bin = str(candidate)
        if (ws / "pyproject.toml").exists() or (ws / "setup.py").exists():
            proc = subprocess.run(
                [py_bin, "-m", "pip", "install", "-e", ".", "-q"],
                cwd=ws,
                capture_output=True,
                text=True,
                timeout=300,
            )
            if proc.returncode != 0:
                # Fallback: requirements.txt only
                req = ws / "requirements.txt"
                if req.exists() and req.stat().st_size > 0:
                    subprocess.run(
                        [py_bin, "-m", "pip", "install", "-r", str(req), "-q"],
                        cwd=ws,
                        capture_output=True,
                        text=True,
                        timeout=300,
                    )
                print(
                    "WARN pip install -e . failed:\n"
                    + (proc.stdout or "")[-300:]
                    + (proc.stderr or "")[-300:],
                    file=sys.stderr,
                )
        return py_bin

    def count_test_methods() -> int:
        n = 0
        for path in list(ws.glob("test_*.py")) + list(ws.glob("tests/**/test_*.py")):
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            n += len(re.findall(r"^\s*def\s+test_\w+", text, flags=re.M))
        return n

    def detect_python_cli_module() -> str | None:
        """Infer `python -m <pkg>` target from workspace layout / pyproject."""
        skip = {
            ".venv",
            ".vibe",
            ".git",
            "__pycache__",
            "node_modules",
            "tests",
            "test",
            "dist",
            "build",
            "target",
        }
        found: list[str] = []
        for p in ws.iterdir():
            if not p.is_dir() or p.name in skip or p.name.endswith(".egg-info"):
                continue
            if not re.match(r"^[A-Za-z_][\w]*$", p.name):
                continue
            if (p / "__init__.py").is_file() and (
                (p / "__main__.py").is_file() or (p / "cli.py").is_file()
            ):
                found.append(p.name)
        if len(found) == 1:
            return found[0]
        for name in found:
            if (ws / name / "__main__.py").is_file():
                return name
        if found:
            return found[0]
        pyproject = ws / "pyproject.toml"
        if pyproject.is_file():
            m = re.search(
                r'(?m)^\s*name\s*=\s*["\']([^"\']+)["\']',
                pyproject.read_text(encoding="utf-8", errors="replace"),
            )
            if m:
                name = m.group(1).replace("-", "_")
                if (ws / name).is_dir():
                    return name
        return None

    for raw in checks:
        kind, _, rest = raw.partition(":")
        ok = False
        detail = rest
        try:
            if kind == "path":
                ok = (ws / rest).exists()
            elif kind == "path_glob":
                ok = any(ws.glob(rest))
            elif kind == "file_contains":
                path_s, _, needle = rest.partition(":")
                text = (ws / path_s).read_text(encoding="utf-8", errors="replace")
                ok = needle.lower() in text.lower()
                detail = f"{path_s} contains {needle!r}"
            elif kind == "has_tests":
                need = int(rest or "1")
                found = count_test_methods()
                ok = found >= need
                detail = f"test methods={found} (need>={need})"
            elif kind == "help_contains":
                # Optional "module:needle"; otherwise auto-detect package CLI.
                if ":" in rest and not rest.startswith(":"):
                    mod, _, needle = rest.partition(":")
                    mod, needle = mod.strip(), needle.strip()
                else:
                    needle = rest.strip()
                    mod = detect_python_cli_module() or ""
                if not mod:
                    ok = False
                    detail = f"-h contains {needle!r} (no python CLI package detected)"
                else:
                    vpy = ensure_venv_and_install()
                    proc = subprocess.run(
                        [vpy, "-m", mod, "-h"],
                        cwd=ws,
                        capture_output=True,
                        text=True,
                        timeout=60,
                    )
                    blob = (proc.stdout or "") + (proc.stderr or "")
                    ok = proc.returncode == 0 and needle.lower() in blob.lower()
                    detail = f"python -m {mod} -h contains {needle!r} (exit={proc.returncode})"
                    if not ok:
                        detail += "\n" + blob[-500:]
            elif kind == "run":
                # Package may declare deps (requests, etc.); install in .venv first.
                if re.search(r"\b(python3?|unittest|pytest)\b", rest):
                    vpy = ensure_venv_and_install()
                    # Only rewrite the leading interpreter token (avoid matching
                    # "python" inside the venv path on a second replace).
                    rest = re.sub(r"^python3?\b", vpy, rest, count=1)
                proc = subprocess.run(
                    rest,
                    shell=True,
                    cwd=ws,
                    capture_output=True,
                    text=True,
                    timeout=120,
                )
                ok = proc.returncode == 0
                detail = f"{rest} -> {proc.returncode}"
                # Unittest with 0 tests is not a pass for suite checkpoints.
                if ok and "unittest" in rest and "Ran 0 tests" in (proc.stdout or ""):
                    ok = False
                    detail += " (Ran 0 tests)"
                if not ok:
                    detail += "\n" + (proc.stdout or "")[-500:] + (proc.stderr or "")[-500:]
            else:
                print(f"FAIL unknown check kind: {raw}")
                failed += 1
                continue
        except Exception as e:
            print(f"FAIL {raw}: {e}")
            failed += 1
            continue
        print(("PASS" if ok else "FAIL"), detail)
        if not ok:
            failed += 1
    if failed:
        print(f"checkpoint {args.checkpoint}: {failed} failed")
        return 1
    print(f"checkpoint {args.checkpoint}: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Compare fiasco-lsp's preprocess against the reference tool on token level.

The standalone implementation keeps the column layout of the source (FPP
keywords are blanked, nothing is reformatted), so its output is never
byte-identical to the reference tool. This script instead compares the C++
token streams of both outputs, ignoring comments, whitespace and `#line`
directives. Declaration specifiers (inline, static, virtual, ...) are compared
order-insensitively within a run.

Usage: tests/semantic_compare.py [--case NAME ...] [--show-diff] -- ADAPTER [ARGS...]
Example: tests/semantic_compare.py -- target/debug/fiasco-lsp preprocess
"""

import argparse
import difflib
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from reference_preprocess import CASES, FIXTURES

REFERENCE = FIXTURES.parent / "src/preprocess"

TOKEN = re.compile(
    r"""
    (?P<ws>\s+)
  | (?P<comment>//[^\n]*|/\*.*?\*/)
  | (?P<string>"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')
  | (?P<word>[A-Za-z_][A-Za-z_0-9]*)
  | (?P<number>[0-9](?:[A-Za-z_0-9.]|'(?=[0-9A-Fa-f]))*)
  | (?P<punct>::|->\*?|<<=?|>>=?|[-+*/%&|^!=<>]=|&&|\|\||\+\+|--|\.\.\.|.)
    """,
    re.S | re.X,
)

SPECIFIERS = {"inline", "static", "virtual", "explicit", "constexpr", "ALWAYS_INLINE"}


def tokens(text):
    lines = [line for line in text.splitlines() if not re.match(r"\s*#\s*line\b", line)]
    result = []
    for match in TOKEN.finditer("\n".join(lines)):
        kind = match.lastgroup
        if kind in ("ws", "comment"):
            continue
        result.append(match.group())
    # Normalize runs of declaration specifiers.
    normalized, run = [], []
    for token in result + [None]:
        if token in SPECIFIERS:
            run.append(token)
            continue
        normalized.extend(sorted(run))
        run = []
        if token is not None:
            normalized.append(token)
    return normalized


def run(command, case, directory):
    sources = [str(FIXTURES / f"{s}.cpp") for s in (case.sources or (case.name,))]
    args = list(case.flags) + ["-l", "-c", case.name] + sources
    return subprocess.run(command + args, cwd=directory, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", action="append")
    parser.add_argument("--show-diff", action="store_true")
    parser.add_argument("adapter", nargs="+")
    options = parser.parse_args()
    adapter = [str(Path(options.adapter[0]).resolve())] + options.adapter[1:]
    failures = 0
    for case in CASES:
        if options.case and case.name not in options.case:
            continue
        with tempfile.TemporaryDirectory() as ref_dir, tempfile.TemporaryDirectory() as our_dir:
            ref = run(["perl", str(REFERENCE)], case, ref_dir)
            ours = run(adapter, case, our_dir)
            problems = []
            if (ref.returncode != 0) != (ours.returncode != 0):
                problems.append(f"exit status {ours.returncode}, reference {ref.returncode}: {ours.stderr.strip()}")
            names = sorted({p.name for p in Path(ref_dir).iterdir()} | {p.name for p in Path(our_dir).iterdir()})
            for name in names:
                ref_path, our_path = Path(ref_dir) / name, Path(our_dir) / name
                if not ref_path.exists() or not our_path.exists():
                    problems.append(f"{name}: only in {'reference' if ref_path.exists() else 'ours'}")
                    continue
                a, b = tokens(ref_path.read_text()), tokens(our_path.read_text())
                if a != b:
                    diff = list(difflib.unified_diff(a, b, "reference", "ours", n=4, lineterm=""))
                    problems.append(f"{name}: token streams differ ({len(diff)} diff lines)")
                    if options.show_diff:
                        problems.append("    " + " ".join(diff[2:200]))
        status = "ok" if not problems else "DIFF"
        failures += bool(problems)
        print(f"{status:4} {case.name}")
        for problem in problems:
            print(f"     {problem}")
    print(f"{failures} case(s) differ")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

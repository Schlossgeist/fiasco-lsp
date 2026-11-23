#!/usr/bin/env python3
"""Compare a preprocess-compatible executable against the upstream Fiasco fixtures."""

import argparse
import difflib
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


FIXTURES = Path(__file__).resolve().parent / "fixtures/fiasco/tool/preprocess/test"


@dataclass(frozen=True)
class Case:
    name: str
    sources: tuple = ()
    flags: tuple = ()
    extra: tuple = ()
    missing: tuple = ()
    failure: bool = False


CASES = (
    Case("mapping"), Case("mapping_inline", ("mapping",), ("-i",)),
    Case("random"), Case("random_inline", ("random",), ("-i",)),
    Case("extern_c"), Case("static", flags=("-i",)),
    Case("noinline", flags=("-i",)), Case("explicit"), Case("operator"),
    Case("template"), Case("template_inline", ("template",), ("-i",)),
    Case("c-preproc"), Case("inline", flags=("-i",)),
    Case("inline_noinline", ("inline",)), Case("parser", flags=("-i",)),
    Case("parser_noinline", ("parser",)),
    Case("multifile", ("multifile1", "multifile2"), ("-i",),
         ("multifile-part1.cc", "multifile-part2.cc")),
    Case("variable"), Case("line"), Case("line_not", ("line",), ("-l",)),
    Case("line_nh", ("line",), ("-L",)),
    Case("interface", extra=("interfacepublic.h",), missing=("interface.h",)),
    Case("comment_in_string"), Case("default_args"),
    Case("drop_single1", ("dropsection",), ("-s", "-e", "bax")),
    Case("drop_single2", ("dropsection",), ("-s", "-e", "bax ixbix")),
    Case("drop_single3", ("dropsection",), ("-s", "-e", "bax aba")),
    Case("drop_multi1", ("dropsection", "dropsection-ext"), ("-s", "-e", "bax aba"), ("drop_multi1-ext.cc",)),
    Case("drop_multi2", ("dropsection", "dropsection-ext"), ("-s", "-e", "bax aba ext"), ("drop_multi2-ext.cc",)),
    Case("implement_template"),
    Case("implement_default", extra=("implement_default.stderr",)),
    Case("implement_default_err", extra=("implement_default_err.stderr",),
         missing=("implement_default_err.h", "implement_default_err_i.h", "implement_default_err.cc"), failure=True),
    Case("nested_class"), Case("template_base_class"), Case("func_defs"),
    Case("extension_inherit"), Case("tag_enabled", flags=("-e", "tag_defined")),
)


def outputs(case):
    return tuple(name for name in (case.name + ".h", case.name + "_i.h", case.name + ".cc")
                 if name not in case.missing) + case.extra


def compare(expected, actual, label):
    if expected == actual:
        return []
    diff = "".join(difflib.unified_diff(
        expected.decode("utf-8", errors="backslashreplace").splitlines(keepends=True),
        actual.decode("utf-8", errors="backslashreplace").splitlines(keepends=True),
        fromfile="verify/" + label, tofile="actual/" + label))
    return [f"{label}: bytes differ\n{diff}"]


def run_case(case, command, fixture_dir=FIXTURES):
    errors = []
    sources = case.sources or (case.name,)
    required = [fixture_dir / "verify" / name for name in outputs(case)]
    required += [fixture_dir / ("combine.pl" if name == "random" else name + ".cpp") for name in sources]
    unavailable = [str(path) for path in required if not path.is_file()]
    if unavailable:
        return ["unavailable fixture(s): " + ", ".join(unavailable)]
    with tempfile.TemporaryDirectory(prefix="fiasco-reference-") as temp:
        work = Path(temp)
        for source in sources:
            if source == "random":
                try:
                    result = subprocess.run(["perl", str(fixture_dir / "combine.pl")], capture_output=True, check=True)
                except (OSError, subprocess.CalledProcessError) as exc:
                    return [f"random fixture generation unavailable: {exc}"]
                (work / "random.cpp").write_bytes(result.stdout)
            else:
                shutil.copyfile(fixture_dir / (source + ".cpp"), work / (source + ".cpp"))
        try:
            result = subprocess.run([*command, *case.flags, "-c", case.name,
                                     *(source + ".cpp" for source in sources)],
                                    cwd=work, capture_output=True, timeout=60)
        except (OSError, subprocess.TimeoutExpired) as exc:
            return [f"implementation unavailable or failed to run: {exc}"]
        if (result.returncode != 0) != case.failure:
            errors.append(f"exit status {result.returncode}, expected {'nonzero' if case.failure else 'zero'}; stderr: {result.stderr.decode(errors='replace')}")
        for name in outputs(case):
            if name.endswith(".stderr"):
                actual = result.stderr
            elif not (work / name).is_file():
                errors.append(f"missing output: {name}")
                continue
            else:
                actual = (work / name).read_bytes()
            errors.extend(compare((fixture_dir / "verify" / name).read_bytes(), actual, name))
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-dir", type=Path, default=FIXTURES)
    parser.add_argument("--case", action="append", choices=[case.name for case in CASES])
    parser.add_argument("command", nargs=argparse.REMAINDER, help="adapter command after --")
    args = parser.parse_args()
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        parser.error("implementation unavailable: provide an adapter command after --")
    executable = shutil.which(command[0])
    if not executable:
        parser.error(f"implementation unavailable: {command[0]}")
    command[0] = str(Path(executable).resolve())
    fixture_dir = args.fixture_dir.resolve()
    failed = False
    for case in CASES:
        if args.case and case.name not in args.case:
            continue
        errors = run_case(case, command, fixture_dir)
        print(f"{'FAIL' if errors else 'PASS'} {case.name}", flush=True)
        for error in errors:
            print(error, file=sys.stderr)
        failed |= bool(errors)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
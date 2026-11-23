#!/usr/bin/env python3
"""Provide the Fiasco build directory shared by all tests that need one.

The build directory is created from the `tests/fixtures/fiasco` submodule (the
same checkout that provides the `preprocess` fixtures) with Fiasco's default
configuration (amd64). Its location is `tests/fixtures/build`, unless the
environment variable `FIASCO_BUILD_DIR` is set.

Usage: tests/fiasco_build.py [--complete] [--jobs N]

Prints the path of the build directory. With `--complete`, the kernel is built
as well, which is needed by tests that require the generated files and
`compile_commands.json` of a full build.
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

TESTS = Path(__file__).resolve().parent
FIASCO = TESTS / "fixtures/fiasco"
BUILD_DIR = Path(os.environ.get("FIASCO_BUILD_DIR", TESTS / "fixtures/build")).resolve()


def configured_build_dir():
    """Returns the build directory, creating and configuring it if needed."""
    if not (FIASCO / "Makefile").is_file():
        raise RuntimeError(f"{FIASCO} is not checked out (run `git submodule update --init --remote --depth 1`)")
    if not (BUILD_DIR / "globalconfig.out").is_file():
        # Fiasco's Makefile refuses to create an existing build directory.
        if BUILD_DIR.exists() and any(BUILD_DIR.iterdir()):
            raise RuntimeError(f"{BUILD_DIR} exists but is not a configured Fiasco build directory")
        if BUILD_DIR.exists():
            BUILD_DIR.rmdir()
        subprocess.run(["make", "-C", str(FIASCO), f"B={BUILD_DIR}"], check=True, stdout=sys.stderr)
    return BUILD_DIR


def complete_build_dir(jobs=None):
    """Returns the build directory after building the kernel in it."""
    build = configured_build_dir()
    subprocess.run(["make", "-C", str(build), f"-j{jobs or os.cpu_count() or 1}"], check=True, stdout=sys.stderr)
    return build


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--complete", action="store_true", help="also build the kernel")
    parser.add_argument("--jobs", "-j", type=int)
    options = parser.parse_args()
    build = complete_build_dir(options.jobs) if options.complete else configured_build_dir()
    print(build)


if __name__ == "__main__":
    main()

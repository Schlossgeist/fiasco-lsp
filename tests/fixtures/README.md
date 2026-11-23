### Reference preprocessing fixtures

`fiasco` is a shallow git submodule of https://github.com/kernkonzept/fiasco
that tracks the tip of its `master` branch. It is a complete
checkout: besides the `tool/preprocess` tree (sources, `test/verify` goldens
and the upstream license in `COPYING`) it provides the kernel sources for the
Fiasco build directory shared by the end-to-end tests (`build`, see
`../fiasco_build.py`). From the repository root:

```sh
git submodule update --init --remote --depth 1
```

Rerun this command to update the checkout to the latest upstream state. The
test inputs then live in `fiasco/tool/preprocess/test`. `random.cpp` is
generated deterministically by the upstream `combine.pl` and is not tracked
upstream.

`cargo test` parses each upstream `.cpp` test input (see
`tests/preprocess_fixtures.rs`) and fails with a hint if the submodule is
missing.

To create the shared build directory (`--complete` also builds the kernel):

```sh
tests/fiasco_build.py [--complete]
```

Run the harness self-tests (Python 3 and Perl required):

```sh
python3 -m unittest discover -s tests -p test_reference_preprocess.py -v
```

To compare the project implementation byte-for-byte:

```sh
python3 tests/reference_preprocess.py -- target/debug/fiasco-lsp preprocess
```

The project implementation deliberately does not reformat source lines (FPP
keywords are blanked to keep the column layout for the LSP line mapping), so
this comparison is expected to fail except for the error case. Use the
token-level comparison instead, which runs the reference tool and
`fiasco-lsp preprocess` on every case and compares the C++ token streams of
the outputs, ignoring comments, whitespace, `#line` directives and the order
of declaration specifiers:

```sh
cd tests && python3 semantic_compare.py [--show-diff] -- ../target/debug/fiasco-lsp preprocess
```

All cases match except `implement_default_err`, for which the reference tool
leaves partial outputs behind (excluded by the byte-level harness).

The adapter must accept the upstream command line: optional `-i`, `-l`,
`-L`, `-s`, and `-e TAGS`, followed by `-c CASE` and one or more relative
`.cpp` input names. It must write the corresponding files in its working
directory and return nonzero for the expected error case. It must emit the
reference diagnostics on standard error for the two diagnostic cases. Each
case runs in a fresh temporary directory; outputs and diagnostics are compared
byte-for-byte, including line directives. Extra arguments can follow the
adapter path. Use `--case mapping` (repeatable) before `--` to select cases,
or `--fixture-dir PATH` to use another upstream test tree. Missing inputs,
goldens, Perl, or an adapter are errors, never passing or skipped cases.

The harness follows the upstream Makefile's output lists: its `missing` entries
are excluded from comparison, not assertions that files are absent; other
unlisted outputs and stderr are not checked. `implement_default_err` expects
nonzero status. The bundled original executable can validate the harness:

```sh
python3 tests/reference_preprocess.py -- tests/fixtures/fiasco/tool/preprocess/src/preprocess
```

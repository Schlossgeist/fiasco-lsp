# Fiasco LSP Proxy
The Fiasco Language Server Protocol proxy transparently translates between an
editor (sees un-preprocessed Fiasco source) and a language server (sees
preprocessed Fiasco), thereby making it possible to use a language server such
as clangd, when reading/(writing) in your favorite LSP-enabled editor.

An inherent limitation, but depending on the situation can also be an advantage, of
this LSP proxy approach is that you are always limited to a single Fiasco
configuration. This means, for example, implementations or references to a
method in other architectures are not displayed.

## Prerequisites
* [Rust](https://www.rust-lang.org/) development environment with cargo
* [clangd](https://clangd.llvm.org/)

## Build
The Fiasco LSP Proxy is a Rust application built with cargo, so nothing special
there.

### Testing
The tests use a single checkout of Fiasco, pulled in as a shallow git submodule
at `tests/fixtures/fiasco`. It provides the upstream test files of Fiasco's
`preprocess` tool as well as the sources for the Fiasco build directory used by
the end-to-end tests. To fetch it, run this in the root directory of this
project:
```bash
git submodule update --init --remote --depth 1
```
The submodule tracks the tip of Fiasco's `master` branch, so rerun this command
to update it to the latest upstream state. Afterward, `cargo test` runs the
parser against every upstream `preprocess` test input.

Tests that need a Fiasco build directory share one at `tests/fixtures/build`
(or `$FIASCO_BUILD_DIR`), which `tests/fiasco_build.py` creates from the
submodule with the default configuration (amd64). Use `--complete` to also
build the kernel, which the ignored Rust tests require:
```bash
tests/fiasco_build.py --complete
cargo test -- --ignored
```
`tests/standalone_e2e.py` is an end-to-end test of the standalone mode with
clangd, which creates the build directory on demand. See
`tests/fixtures/README.md` for the reference comparison harnesses.

## Usage
You have to either tell the LSP proxy a path to a Fiasco build directory using
the `--build-dir` option. Or alternatively a path to the Fiasco source dir via
`--fiasco-dir`, a Fiasco `globalconfig.out` via `--fiasco-config` and optionally
a `Makeconf.local` via `--makeconf`.

Then, depending on your editor's preferences, it can communicate with the LSP
proxy via:
- stdin/stdout (default)
- `--connect <port>`: Connect to LSP-enabled editor on port
- `--listen <port>`: Listen for LSP-enabled editor on port

Use `--clangd <path>` to select the language server executable (default:
`clangd`).

By default, nothing is logged. Log output is never written to stdout, which
carries the LSP communication when using stdin/stdout, or to stderr. Use
`--log-output <path>` to append it to a file, or `--log-output <host>:<port>`
(e.g. `--log-output localhost:1337`) to send it to a TCP socket, for example to
`nc -lk 1337`. The connection is established on demand, so the listener can be
started or restarted at any time; until it is available, the most recent log
output (up to 1 MiB) is buffered. To write to a file whose name looks like
`<host>:<port>`, prefix it with a directory, e.g. `./localhost:1337`. The
stderr output of clangd is forwarded to the log, and panics are logged in
addition to the panic report on stderr. stderr is never redirected.

Logging is configured via `RUST_LOG` as usual; the default is
`info,goto=debug`, which includes the raw and mapped responses of goto
requests (go to definition/declaration/...). Use e.g. `RUST_LOG=info` to
disable those.

Diagnostics whose message matches a regular expression given with
`--suppress-diagnostic <regex>` (repeatable) are not reported to the editor.
This is useful for errors about compiler flags clang does not support, as
Fiasco is built with GCC, e.g. `--suppress-diagnostic '^Unsupported option'`
for `-freg-struct-return` on amd64.

### Standalone mode
With `--standalone`, the proxy does not depend on the output of Fiasco's Perl
`preprocess` tool. Instead, it contains its own implementation, based on the
[tree-sitter-fpp](https://github.com/Schlossgeist/tree-sitter-fpp) grammar,
which re-runs on every change of a source file:

1. At startup, Fiasco's build system is run with `PREPROCESS` pointing to
   `fiasco-lsp preprocess` (see below). This generates `auto/` and
   `compile_commands.json` as usual, and records every preprocess invocation
   (module, flags, enabled tags, input files), so module composition, `VPATH`
   resolution and tag selection stay entirely in the hands of the build system.
2. Each change of an opened source file regenerates its module in memory. The
   updated generated files are sent to the language server, written to `auto/`
   (so the language server also sees current versions of headers that are not
   open), and the line mapping is updated.
3. Diagnostics of the preprocessing itself (e.g. `Class X has not been
   declared`) are reported alongside the language server's diagnostics.

The generated code never reformats source lines: FPP keywords such as `PUBLIC`
or `NEEDS[...]`, class qualifiers in synthesized member declarations, or default
arguments in definitions are replaced by blanks, and `TAG_ENABLED(...)` by an
equally wide `true`/`false`. Hence every generated line corresponds to one
source line with identical columns, including the in-class declarations
synthesized from `PUBLIC`/`PROTECTED`/`PRIVATE` definitions.

Note that standalone mode writes the generated files to the `auto/` directory of
the build directory, like a regular build would (without `#line` directives).

### Preprocess command
`fiasco-lsp preprocess [options] files...` is a drop-in replacement for Fiasco's
`tool/preprocess/src/preprocess` with the same command line options, so it can
also be used for building Fiasco: `make PREPROCESS=/path/to/wrapper`, where the
wrapper script runs `exec fiasco-lsp preprocess "$@"`. For the default amd64
configuration, all generated files match the reference tool on token level
(except for the order of equivalent specifiers in three files), and the kernel
built with it is functionally identical.

## What Works
- Navigation (e.g. goto, find references, ...)
- Code diagnostics
- Inlay hints
- Code actions suggested by the language server can be executed (experimental)
- Source code changes that do not change the number of lines in the file, thus
  do not require an update of the line mapping, are possible (experimental)
- Arbitrary source code changes with `--standalone`

## Next Steps
- Synchronization of source file changes without `--standalone` (would require
  re-executing the external preprocess tool).
- Implement support for more LSP requests/responses.
- Build system integration
- Support multiple configurations at same time?

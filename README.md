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
to update it to the latest upstream state. Afterward, `cargo test` parses every upstream `preprocess` test input with the
tree-sitter-fpp grammar.

Tests that need a Fiasco build directory share one at `tests/fixtures/build`
(or `$FIASCO_BUILD_DIR`), which `tests/fiasco_build.py` creates from the
submodule with the default configuration (amd64). Use `--complete` to also
build the kernel, which the ignored Rust tests require:
```bash
tests/fiasco_build.py --complete
cargo test -- --ignored
```
`tests/standalone_e2e.py` creates the build directory on demand. See
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

## What Works
- Navigation (e.g. goto, find references, ...)
- Code diagnostics
- Inlay hints
- Code actions suggested by the language server can be executed (experimental)
- Source code changes that do not change the number of lines in the file, thus
  do not require an update of the line mapping, are possible (experimental)

## Next Steps
- Synchronization of source file changes made in text editor, with the language
  server and the internal mapping of the LSP proxy. The biggest hurdle here is
  certainly the correct synchronization of the internal mapping used by the LSP
  proxy, as it requires a re-execution of preprocess.
- Implement support for more LSP requests/responses.
- Build system integration
- Support multiple configurations at same time?

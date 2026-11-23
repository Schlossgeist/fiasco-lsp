#!/usr/bin/env python3
"""End-to-end test of `fiasco-lsp --standalone` with clangd on a real Fiasco build.

Usage:
  tests/standalone_e2e.py [--build-dir BUILD] [--clangd clangd] [--proxy target/debug/fiasco-lsp]

BUILD must be a configured Fiasco build directory. By default, the build
directory shared by all tests is used, which is created from the
`tests/fixtures/fiasco` submodule if needed (see `tests/fiasco_build.py`).
The test edits
`kern/ram_quota.cpp` in the editor only (the file on disk is not modified)
and checks navigation, line mapping after edits, and diagnostics. The log
output of the proxy is sent to a socket (`--log-output HOST:PORT`) and stdout
must only carry LSP messages.
"""

import argparse
import json
import queue
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import fiasco_build


class LogListener:
    """Collects everything sent to a local TCP socket."""

    def __init__(self):
        self.server = socket.create_server(("127.0.0.1", 0))
        self.address = "127.0.0.1:%d" % self.server.getsockname()[1]
        self.data = bytearray()
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self):
        while True:
            connection, _ = self.server.accept()
            with connection:
                while chunk := connection.recv(65536):
                    self.data += chunk


class Client:
    def __init__(self, command, root):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr)
        self.messages = queue.Queue()
        self.next_id = 1
        self.versions = {}
        self.protocol_errors = []
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {
            "processId": None, "rootUri": root.as_uri(),
            "capabilities": {"textDocument": {"publishDiagnostics": {}, "definition": {}}},
        }, timeout=600)
        self.notify("initialized", {})

    def _read(self):
        # Anything on stdout that is not an LSP message (e.g. log output) is a
        # protocol error.
        stream = self.process.stdout
        while True:
            headers = {}
            while True:
                line = stream.readline()
                if not line:
                    return
                if line == b"\r\n":
                    break
                key, separator, value = line.decode(errors="replace").partition(":")
                if not separator or not line.endswith(b"\r\n"):
                    self.protocol_errors.append(line)
                    continue
                headers[key.lower()] = value.strip()
            try:
                self.messages.put(json.loads(stream.read(int(headers["content-length"]))))
            except (KeyError, ValueError) as e:
                self.protocol_errors.append(f"{e}: {headers}")

    def _send(self, message):
        body = json.dumps({"jsonrpc": "2.0", **message}).encode()
        self.process.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        self.process.stdin.flush()

    def notify(self, method, params):
        self._send({"method": method, "params": params})

    def receive(self, predicate, timeout=120):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                message = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            if message.get("method") == "window/workDoneProgress/create" or (
                    "id" in message and "method" in message):
                self._send({"id": message["id"], "result": None})
                continue
            if predicate(message):
                return message
        raise TimeoutError("timed out waiting for message")

    def request(self, method, params, timeout=120):
        request_id = self.next_id
        self.next_id += 1
        self._send({"id": request_id, "method": method, "params": params})
        return self.receive(lambda m: m.get("id") == request_id and "method" not in m, timeout).get("result")

    def open(self, path, text):
        self.versions[path] = 1
        self.notify("textDocument/didOpen", {"textDocument": {
            "uri": path.as_uri(), "languageId": "cpp", "version": 1, "text": text}})

    def change(self, path, line, character, text, end=None):
        self.versions[path] += 1
        end_line, end_character = end or (line, character)
        self.notify("textDocument/didChange", {
            "textDocument": {"uri": path.as_uri(), "version": self.versions[path]},
            "contentChanges": [{"range": {"start": {"line": line, "character": character},
                                          "end": {"line": end_line, "character": end_character}},
                                "text": text}]})

    def definition(self, path, line, character):
        result = self.request("textDocument/definition", {
            "textDocument": {"uri": path.as_uri()}, "position": {"line": line, "character": character}})
        if isinstance(result, dict):
            result = [result]
        return [(r.get("uri") or r.get("targetUri"), (r.get("range") or r.get("targetSelectionRange"))["start"]["line"])
                for r in result or []]

    def diagnostics(self, path, predicate, timeout=120):
        return self.receive(lambda m: m.get("method") == "textDocument/publishDiagnostics"
                            and m["params"]["uri"] == path.as_uri()
                            and predicate(m["params"]["diagnostics"]), timeout)["params"]["diagnostics"]

    def shutdown(self):
        try:
            self.request("shutdown", None, timeout=10)
            self.notify("exit", None)
            self.process.wait(timeout=10)
        except Exception:
            self.process.kill()


def check(condition, message):
    print(("ok   " if condition else "FAIL ") + message)
    return bool(condition)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--build-dir", type=Path, help="default: the shared test build directory")
    parser.add_argument("--clangd", default="clangd")
    parser.add_argument("--proxy", default=str(Path(__file__).resolve().parents[1] / "target/debug/fiasco-lsp"))
    options = parser.parse_args()
    build = (options.build_dir or fiasco_build.configured_build_dir()).resolve()

    started = time.monotonic()
    log = LogListener()
    # Fiasco is built with GCC; clang rejects some of its flags (e.g. `-freg-struct-return` on x86-64).
    client = Client([options.proxy, "--build-dir", str(build), "--standalone", "--clangd", options.clangd,
                     "--suppress-diagnostic", "^Unsupported option", "--log-output", log.address], build)
    print(f"initialized after {time.monotonic() - started:.1f}s")
    # The `source` link is created by the build system during startup.
    source = (build / "source").resolve() / "kern/ram_quota.cpp"
    text = source.read_text()
    lines = text.splitlines(keepends=True)
    ok = True
    try:
        client.open(source, text)
        diagnostics = client.diagnostics(source, lambda d: True, timeout=300)
        errors = [d for d in diagnostics if d.get("severity") == 1]
        ok &= check(not errors, f"no errors in unmodified source: {[(d['range']['start']['line'], d['message']) for d in errors]}")

        # `_current` in `{ return _current & ~Invalid; }` (line 62) -> member declaration (line 15).
        current_line = next(i for i, l in enumerate(lines) if "return _current & ~Invalid" in l)
        member_line = next(i for i, l in enumerate(lines) if "Mword _current;" in l)
        column = lines[current_line].index("_current") + 2
        ok &= check((source.as_uri(), member_line) in client.definition(source, current_line, column),
                    "definition of member maps back into the source")

        # A class of another module: its generated header is not open in
        # clangd, but the location must still map back to its .cpp source.
        root_line = next(i for i, l in enumerate(lines) if "static Global_data<Ram_quota *> root;" in l)
        targets = client.definition(source, root_line, lines[root_line].index("Global_data") + 2)
        ok &= check(targets and all(uri.endswith("/global_data.cpp") for uri, _ in targets),
                    f"definition in another module maps to its source: {targets}")

        # Locations in plain headers are passed through unchanged.
        call_line = next(i for i, l in enumerate(lines) if "Mword o = access_once(&_current);" in l)
        targets = client.definition(source, call_line, lines[call_line].index("access_once") + 2)
        ok &= check(targets and all(uri.endswith("/types.h") for uri, _ in targets),
                    f"definition in a plain header is kept: {targets}")

        # Insert three lines (one new member function): mapping must follow.
        insert_at = next(i for i, l in enumerate(lines) if l.startswith("IMPLEMENT inline Ram_quota::~Ram_quota")) + 1
        client.change(source, insert_at, 0, "PUBLIC int Ram_quota::added() const\n{ return _current; }\n\n")
        time.sleep(1)
        ok &= check((source.as_uri(), member_line) in client.definition(source, current_line + 3, column),
                    "definition still correct for shifted lines after inserting lines")
        ok &= check((source.as_uri(), member_line) in client.definition(source, insert_at + 1, 10),
                    "definition works inside the newly added function")

        # A C++ error in the edited source is reported at the right line.
        error_line = insert_at + 1
        client.change(source, error_line, 2, "undeclared_identifier + ")
        diagnostics = client.diagnostics(source, lambda d: any("undeclared_identifier" in x["message"] for x in d))
        lines_with_error = [d["range"]["start"]["line"] for d in diagnostics if "undeclared_identifier" in d["message"]]
        ok &= check(lines_with_error == [error_line], f"clangd error mapped to line {error_line} ({lines_with_error})")

        # A preprocess error (unknown class) is reported as well.
        client.change(source, insert_at, 0, "PUBLIC void Unknown_class::f() {}\n")
        diagnostics = client.diagnostics(source, lambda d: any(x.get("source") == "preprocess" for x in d))
        ok &= check(any("Unknown_class" in d["message"] and d["range"]["start"]["line"] == insert_at
                        for d in diagnostics), "preprocess error reported at the right line")
    except TimeoutError as e:
        ok = check(False, f"timeout: {e}")
    finally:
        client.shutdown()
    ok &= check(not client.protocol_errors, f"stdout only carries LSP messages: {client.protocol_errors[:5]}")
    ok &= check(b"Fiasco LSP Proxy" in log.data and b"Recorded" in log.data,
                f"log output received via socket ({len(log.data)} bytes)")
    print(f"total {time.monotonic() - started:.1f}s")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

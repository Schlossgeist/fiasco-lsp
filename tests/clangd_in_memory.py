#!/usr/bin/env python3
"""Exercise clangd with generated translation units and headers that do not exist on disk."""

import json
import os
import queue
import subprocess
import threading
import unittest
from pathlib import Path


class ClangdInMemoryTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(__file__).resolve().parent
        self.header_path = self.root / "clangd-virtual-generated.h"
        self.source_path = self.root / "clangd-virtual-generated.cc"
        self.assertFalse(self.header_path.exists())
        self.assertFalse(self.source_path.exists())
        self.header = self.header_path.as_uri()
        self.source = self.source_path.as_uri()
        self.process = subprocess.Popen(
            ["clangd", "--background-index=0", "--log=error"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        self.messages = queue.Queue()
        threading.Thread(target=self.read_messages, daemon=True).start()
        self.send({"id": 1, "method": "initialize", "params": {
            "processId": os.getpid(), "rootUri": self.root.as_uri(), "capabilities": {},
            "initializationOptions": {"fallbackFlags": ["-std=c++17", "-DTEST_FLAG=42", "-I", str(self.root)]},
        }})
        self.receive(lambda m: m.get("id") == 1)
        self.send({"method": "initialized", "params": {}})

    def tearDown(self):
        self.process.terminate()
        self.process.wait(timeout=5)
        self.process.stdin.close()
        self.process.stdout.close()

    def read_messages(self):
        stream = self.process.stdout
        while True:
            headers = {}
            while True:
                line = stream.readline()
                if not line:
                    return
                if line in (b"\r\n", b"\n"):
                    break
                key, value = line.decode().split(":", 1)
                headers[key.lower()] = value.strip()
            self.messages.put(json.loads(stream.read(int(headers["content-length"]))))

    def send(self, message):
        body = json.dumps({"jsonrpc": "2.0", **message}).encode()
        self.process.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        self.process.stdin.flush()

    def receive(self, predicate):
        import time
        deadline = time.monotonic() + 15
        seen = []
        while time.monotonic() < deadline:
            try:
                message = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            seen.append(message)
            if predicate(message):
                return message
        self.fail(f"timed out waiting for clangd; received: {seen}")

    def open_document(self, uri, text):
        self.send({"method": "textDocument/didOpen", "params": {"textDocument": {
            "uri": uri, "languageId": "cpp", "version": 1, "text": text,
        }}})

    def test_virtual_header_diagnostics_and_definition(self):
        self.open_document(self.header, "#pragma once\nint generated_value();\n")
        self.open_document(self.source, '#include "clangd-virtual-generated.h"\nint main() { return generated_value(); }\n')
        diagnostics = self.receive(lambda m: m.get("method") == "textDocument/publishDiagnostics"
                                   and m["params"]["uri"] == self.source)
        self.send({"id": 2, "method": "textDocument/definition", "params": {
            "textDocument": {"uri": self.source}, "position": {"line": 1, "character": 22},
        }})
        definition = self.receive(lambda m: m.get("id") == 2)
        with self.subTest("diagnostics"):
            self.assertEqual(diagnostics["params"]["diagnostics"], [], diagnostics)
        with self.subTest("definition"):
            self.assertTrue(any(location.get("uri") == self.header
                                for location in definition.get("result") or []), definition)
        self.assertFalse(self.header_path.exists())
        self.assertFalse(self.source_path.exists())

    def test_virtual_source_receives_compile_flags(self):
        self.open_document(self.source, "static_assert(TEST_FLAG == 42);\n")
        diagnostics = self.receive(lambda m: m.get("method") == "textDocument/publishDiagnostics"
                                   and m["params"]["uri"] == self.source)
        self.assertEqual(diagnostics["params"]["diagnostics"], [], diagnostics)
        self.assertFalse(self.source_path.exists())


if __name__ == "__main__":
    unittest.main()
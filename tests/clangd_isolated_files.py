#!/usr/bin/env python3
"""Exercise clangd with generated files in an isolated directory."""

import tempfile
import sys
import unittest
from pathlib import Path

from clangd_in_memory import ClangdInMemoryTest


class ClangdIsolatedFilesTest(unittest.TestCase):
    setUp = ClangdInMemoryTest.setUp
    tearDown = ClangdInMemoryTest.tearDown
    read_messages = ClangdInMemoryTest.read_messages
    send = ClangdInMemoryTest.send
    receive = ClangdInMemoryTest.receive
    open_document = ClangdInMemoryTest.open_document
    def test_isolated_header_navigation_diagnostics_and_update(self):
        with tempfile.TemporaryDirectory(dir=self.root, prefix="clangd-isolated-") as directory:
            root = Path(directory)
            header_path = root / "generated.h"
            source_path = root / "generated.cc"
            header = header_path.as_uri()
            source = source_path.as_uri()
            header_path.write_text("#pragma once\nint generated_value();\n")
            text = '#include "generated.h"\nstatic_assert(TEST_FLAG == 42);\nint main() { return generated_value(); }\n'
            source_path.write_text(text)
            self.open_document(source, text)
            diagnostics = self.receive(lambda m: m.get("method") == "textDocument/publishDiagnostics"
                                       and m["params"]["uri"] == source)
            self.assertEqual(diagnostics["params"]["diagnostics"], [], diagnostics)
            self.send({"id": 2, "method": "textDocument/definition", "params": {
                "textDocument": {"uri": source}, "position": {"line": 2, "character": 22},
            }})
            definition = self.receive(lambda m: m.get("id") == 2)
            self.assertTrue(any(location.get("uri") == header
                                for location in definition.get("result") or []), definition)

            header_path.write_text("#pragma once\nint replacement();\n")
            self.send({"method": "workspace/didChangeWatchedFiles", "params": {
                "changes": [{"uri": header, "type": 2}],
            }})
            self.send({"method": "textDocument/didChange", "params": {
                "textDocument": {"uri": source, "version": 2},
                "contentChanges": [{"text": text}],
            }})
            diagnostics = self.receive(lambda m: m.get("method") == "textDocument/publishDiagnostics"
                                       and m["params"]["uri"] == source
                                       and m["params"].get("version") == 2)
            self.assertTrue(any("generated_value" in diagnostic["message"]
                                for diagnostic in diagnostics["params"]["diagnostics"]), diagnostics)


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ClangdIsolatedFilesTest)
    sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
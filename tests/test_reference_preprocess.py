import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import reference_preprocess as reference


class ReferenceHarnessTest(unittest.TestCase):
    def test_upstream_reference_matches_all_cases(self):
        tool = reference.FIXTURES.parent / "src/preprocess"
        self.assertTrue(tool.is_file(), "reference tool unavailable")
        for case in reference.CASES:
            with self.subTest(case=case.name):
                self.assertEqual(reference.run_case(case, [str(tool)]), [])

    def test_missing_fixture_is_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            errors = reference.run_case(reference.Case("mapping"), [sys.executable], Path(temp))
        self.assertIn("unavailable fixture", errors[0])

    def test_missing_implementation_is_failure(self):
        errors = reference.run_case(reference.Case("mapping"), ["/nonexistent/preprocess"])
        self.assertIn("implementation unavailable", errors[0])

    def test_wrong_output_and_status_are_detected(self):
        errors = reference.run_case(reference.Case("mapping"), [sys.executable, "-c", "import sys; sys.exit(1)"])
        self.assertTrue(any("exit status" in error for error in errors))
        self.assertTrue(any("missing output" in error for error in errors))
        self.assertTrue(reference.compare(b"expected\n", b"actual\n", "file"))

    def test_no_adapter_is_an_error(self):
        result = subprocess.run([sys.executable, str(Path(reference.__file__))], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"implementation unavailable", result.stderr)


if __name__ == "__main__":
    unittest.main()
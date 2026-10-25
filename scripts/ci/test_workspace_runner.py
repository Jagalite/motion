"""Pure runner regressions; these do not invoke Cargo or external media tools."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("workspace_runner", Path(__file__).with_name("test_workspace.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class WorkspaceRunnerTests(unittest.TestCase):
    def test_full_suite_has_no_name_or_target_filter_and_keeps_lock(self):
        build, test = runner.commands()
        self.assertIn("--locked", build)
        self.assertEqual(build[:len(test)], test)
        self.assertEqual(build[len(test):], ["--no-run", "--message-format=json-render-diagnostics"])
        self.assertNotIn("--bin", build)
        self.assertIn("--locked", test)
        self.assertIn("--workspace", test)
        self.assertIn("--no-fail-fast", test)
        self.assertNotIn("--test", test)
        self.assertNotIn("--lib", test)
        self.assertNotIn("--all-targets", test)  # ordinary test includes doc tests
        self.assertNotIn("--", test)
        self.assertEqual(test[test.index("--profile") + 1], "native-qualification")

    def test_sequential_default_preserves_explicit_override_and_input(self):
        original = {"PATH": "/toolchain"}
        self.assertEqual(runner.qualification_environment(original),
                         {"PATH": "/toolchain", "RUST_TEST_THREADS": "1"})
        self.assertEqual(original, {"PATH": "/toolchain"})
        explicit = {"RUST_TEST_THREADS": "2", "CARGO_BUILD_JOBS": "1"}
        self.assertEqual(runner.qualification_environment(explicit), explicit)

    def test_observer_environment_uses_only_verified_actual_artifact(self):
        with tempfile.TemporaryDirectory() as temp:
            helper = Path(temp) / "playscale"
            helper.write_bytes(b"actual artifact")
            original = {"MOTION_TEST_OBSERVER": "/unverified/harness", "RUST_TEST_THREADS": "1"}
            actual, proof = runner.verified_test_environment(original, helper)
            self.assertEqual(actual["MOTION_TEST_OBSERVER"], str(helper.resolve()))
            self.assertEqual(proof["path"], actual["MOTION_TEST_OBSERVER"])
            self.assertEqual(original["MOTION_TEST_OBSERVER"], "/unverified/harness")
            self.assertEqual(actual["RUST_TEST_THREADS"], "1")
            helper.write_bytes(b"")
            with self.assertRaises(RuntimeError):
                runner.verified_test_environment(original, helper)

    def test_helper_digest_and_empty_or_oversized_refusal(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "helper"
            path.write_bytes(b"fixture")
            proof = runner.verify_helper(path)
            self.assertEqual(proof["bytes"], 7)
            self.assertEqual(len(proof["sha256"]), 64)
            path.write_bytes(b"")
            with self.assertRaises(RuntimeError):
                runner.verify_helper(path)
            with path.open("wb") as file:
                file.truncate(runner.MAX_HELPER_BYTES + 1)
            with self.assertRaises(RuntimeError):
                runner.verify_helper(path)


if __name__ == "__main__":
    unittest.main()

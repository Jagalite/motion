#!/usr/bin/env python3
"""Finite stdlib-only helper tests. Never runs Cargo or changes the source pair."""
from contextlib import redirect_stderr, redirect_stdout
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("paired_local_build", Path(__file__).with_name("paired_local_build.py"))
helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helper)
SOURCE_ROOT = Path(os.environ.get("PAIRED_BUILD_TEST_SOURCE", Path(__file__).resolve().parents[2]))
TEMP_ROOT = os.environ.get("PAIRED_BUILD_TEST_TEMP")


def original_input(name):
    path = SOURCE_ROOT / "motion" / name
    backup = SOURCE_ROOT / "motion" / helper.STATE_NAME / (name + ".original")
    return (backup if backup.is_file() else path).read_bytes()


def synthetic_cargo_lock(original):
    """Text fixture matches the already-observed Cargo-normalized lock hash.

    This fixture is test data only. The helper never writes it as a build lock.
    """
    text = original.decode()
    for name in helper.PATHS:
        prefix = f'[[package]]\nname = "{name}"\n'
        start = text.index(prefix)
        end = text.find('[[package]]', start + len(prefix))
        if end == -1:
            end = len(text)
        section = text[start:end]
        section = section.replace(f'source = "{helper.SOURCE}"\n', '', 1)
        text = text[:start] + section + text[end:]
    for name, old, new in (
        ("catabolic-runtime", ' "serde_json",', ' "serde",\n "serde_json",'),
        ("motion-catalog-backend", ' "playscale-core",', ' "libc",\n "playscale-core",\n "rusqlite",'),
    ):
        start = text.index(f'[[package]]\nname = "{name}"\n')
        end = text.index('[[package]]', start + 12)
        section = text[start:end]
        assert section.count(old) == 1
        text = text[:start] + section.replace(old, new, 1) + text[end:]
    result = text.encode()
    assert hashlib.sha256(result).hexdigest() == "c9a4afdefc92adf8fc34c218453572520864354839bcda3571438fe7074f3cef"
    return result


class HandoffTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original_manifest = original_input("Cargo.toml")
        cls.original_lock = original_input("Cargo.lock")
        helper.expect_hash(cls.original_manifest, helper.ORIGINAL_MANIFEST_SHA256, "test fixture manifest")
        helper.expect_hash(cls.original_lock, helper.ORIGINAL_LOCK_SHA256, "test fixture lock")
        cls.local_lock = synthetic_cargo_lock(cls.original_lock)
        cls.paired_files = {name: (SOURCE_ROOT / name).read_bytes() for name in helper.PAIRED_MANIFESTS}

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="paired-build-test-", dir=TEMP_ROOT)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.motion = self.root / "motion"
        self.motion.mkdir()
        (self.motion / "Cargo.toml").write_bytes(self.original_manifest)
        (self.motion / "Cargo.lock").write_bytes(self.original_lock)
        for name, data in self.paired_files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.pair = helper.Pair(self.motion)

    def snapshot(self):
        return {str(p.relative_to(self.root)): p.read_bytes() for p in self.root.rglob("*") if p.is_file()}

    def test_status_read_only_default(self):
        before = self.snapshot()
        with redirect_stdout(io.StringIO()):
            self.assertEqual(helper.main(["--motion-dir", str(self.motion)]), 0)
        self.assertEqual(self.snapshot(), before)

    def test_prepare_exact_four_changes_and_backups(self):
        self.pair.prepare()
        expected = self.original_manifest
        for name, sub in helper.PATHS.items():
            expected = expected.replace(
                f'{name} = {{ git = "{helper.GIT}", rev = "{helper.REV}" }}'.encode(),
                f'{name} = {{ path = "../catabolic/{sub}" }}'.encode(),
            )
        self.assertEqual(self.pair.inputs(), (expected, self.original_lock))
        self.assertEqual(self.pair.backups(), (self.original_manifest, self.original_lock, None))
        for name, data in self.paired_files.items():
            self.assertEqual((self.root / name).read_bytes(), data)

    def test_prepare_is_idempotent(self):
        self.pair.prepare()
        before = self.snapshot()
        self.pair.prepare()
        self.assertEqual(self.snapshot(), before)

    def test_round_trip_and_repeated_cycle(self):
        for _ in range(2):
            self.pair.prepare()
            (self.motion / "Cargo.lock").write_bytes(self.local_lock)
            self.pair.verify_lock()
            before = self.snapshot()
            self.pair.verify_lock()
            self.pair.prepare()
            self.assertEqual(self.snapshot(), before)
            self.assertEqual(self.pair.inspect(), "localized-verified")
            self.pair.restore()
            self.assertEqual(self.pair.inputs(), (self.original_manifest, self.original_lock))
            before = self.snapshot()
            self.pair.restore()
            self.assertEqual(self.snapshot(), before)

    def test_restore_before_normalization(self):
        self.pair.prepare()
        self.pair.restore()
        self.assertEqual(self.pair.inputs(), (self.original_manifest, self.original_lock))

    def test_refuse_custom_manifest_before_prepare(self):
        (self.motion / "Cargo.toml").write_bytes(self.original_manifest + b"\n# custom\n")
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.prepare()
        self.assertEqual(self.snapshot(), before)

    def test_refuse_custom_lock_before_prepare(self):
        (self.motion / "Cargo.lock").write_bytes(self.original_lock + b"\n# custom\n")
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.prepare()
        self.assertEqual(self.snapshot(), before)

    def test_refuse_paired_manifest_change(self):
        path = self.root / next(iter(helper.PAIRED_MANIFESTS))
        path.write_bytes(path.read_bytes() + b"\n# custom\n")
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.prepare()
        self.assertEqual(self.snapshot(), before)

    def test_refuse_missing_paired_source(self):
        (self.root / "catabolic/crates/catabolic-runtime/Cargo.toml").unlink()
        with self.assertRaises(helper.Refusal):
            self.pair.prepare()
        self.assertFalse(self.pair.state.exists())

    def test_refuse_symlinked_manifest(self):
        path = self.motion / "Cargo.toml"
        outside = self.root / "other.toml"
        outside.write_bytes(path.read_bytes())
        path.unlink()
        path.symlink_to(outside)
        with self.assertRaises(helper.Refusal):
            self.pair.prepare()
        self.assertEqual(outside.read_bytes(), self.original_manifest)

    def test_refuse_symlinked_pair_directory(self):
        path = self.root / "catabolic"
        outside = self.root / "other-catabolic"
        path.rename(outside)
        path.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(helper.Refusal):
            self.pair.prepare()
        self.assertFalse(self.pair.state.exists())

    def test_refuse_symlinked_state(self):
        outside = self.root / "other-state"
        outside.mkdir()
        self.pair.state.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(helper.Refusal):
            helper.Pair(self.motion)

    def test_refuse_existing_incomplete_backups(self):
        self.pair.state.mkdir()
        (self.pair.state / "Cargo.toml.original").write_bytes(self.original_manifest)
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.prepare()
        self.assertEqual(self.snapshot(), before)

    def test_failed_backup_does_not_change_inputs(self):
        real = helper.write_new
        def fail_lock(path, data):
            if path.name == "Cargo.lock.original":
                raise OSError("simulated full disk")
            return real(path, data)
        with patch.object(helper, "write_new", side_effect=fail_lock):
            with self.assertRaises(OSError):
                self.pair.prepare()
        self.assertEqual(self.pair.inputs(), (self.original_manifest, self.original_lock))
        self.assertFalse((self.pair.state / "READY").exists())

    def test_guard_blocks_parallel_helper(self):
        guard = self.motion / ".paired-local-build.guard"
        guard.write_text("existing operation\n")
        before = self.snapshot()
        with redirect_stderr(io.StringIO()):
            self.assertEqual(helper.main(["prepare", "--motion-dir", str(self.motion)]), 2)
        self.assertEqual(self.snapshot(), before)

    def test_guard_is_removed_after_refusal(self):
        (self.motion / "Cargo.lock").write_bytes(b"custom\n")
        with redirect_stderr(io.StringIO()):
            self.assertEqual(helper.main(["prepare", "--motion-dir", str(self.motion)]), 2)
        self.assertFalse((self.motion / ".paired-local-build.guard").exists())

    def test_refuse_lock_without_normalization(self):
        self.pair.prepare()
        with self.assertRaises(helper.Refusal):
            self.pair.verify_lock()
        self.assertFalse((self.pair.state / "Cargo.lock.local").exists())

    def test_refuse_normalized_version_drift(self):
        self.pair.prepare()
        changed = self.local_lock.replace(b'version = "1.0.0"', b'version = "1.0.1"', 1)
        (self.motion / "Cargo.lock").write_bytes(changed)
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.verify_lock()
        self.assertEqual(self.snapshot(), before)

    def test_refuse_unrelated_source_or_checksum_drift(self):
        for old, new in ((b"registry+", b"otherreg+"), (b"checksum = \"", b"checksum = \"0")):
            with self.subTest(old=old):
                changed = self.local_lock.replace(old, new, 1)
                with self.assertRaises(helper.Refusal):
                    helper.validate_local_lock(self.original_lock, changed)

    def test_refuse_missing_required_dependency_edge(self):
        for line in (b' "serde",\n', b' "libc",\n', b' "rusqlite",\n'):
            name = b'catabolic-runtime' if line == b' "serde",\n' else b'motion-catalog-backend'
            start = self.local_lock.index(b'name = "' + name + b'"')
            index = self.local_lock.index(line, start)
            changed = self.local_lock[:index] + self.local_lock[index + len(line):]
            with self.subTest(line=line), self.assertRaises(helper.Refusal):
                helper.validate_local_lock(self.original_lock, changed)

    def test_refuse_unrecorded_lock_on_restore(self):
        self.pair.prepare()
        (self.motion / "Cargo.lock").write_bytes(self.local_lock)
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.restore()
        self.assertEqual(self.snapshot(), before)

    def test_refuse_custom_manifest_on_restore(self):
        self.pair.prepare()
        path = self.motion / "Cargo.toml"
        path.write_bytes(path.read_bytes() + b"\n# custom\n")
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.restore()
        self.assertEqual(self.snapshot(), before)

    def test_refuse_custom_recorded_lock_on_restore(self):
        self.pair.prepare()
        (self.motion / "Cargo.lock").write_bytes(self.local_lock)
        self.pair.verify_lock()
        (self.motion / "Cargo.lock").write_bytes(self.local_lock + b"\n# custom\n")
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.restore()
        self.assertEqual(self.snapshot(), before)

    def test_restore_possible_after_pair_changes(self):
        self.pair.prepare()
        (self.root / "catabolic/Cargo.toml").write_bytes(b"changed after prepare\n")
        self.pair.restore()
        self.assertEqual(self.pair.inputs(), (self.original_manifest, self.original_lock))

    def test_partial_restore_can_retry(self):
        self.pair.prepare()
        (self.motion / "Cargo.lock").write_bytes(self.local_lock)
        self.pair.verify_lock()
        real = helper.replace_checked
        def fail_manifest(path, before, after):
            if path.name == "Cargo.toml":
                raise OSError("simulated interruption")
            return real(path, before, after)
        with patch.object(helper, "replace_checked", side_effect=fail_manifest):
            with self.assertRaises(OSError):
                self.pair.restore()
        self.assertEqual(self.pair.inspect(), "localized-awaiting-lock")
        self.pair.restore()
        self.assertEqual(self.pair.inputs(), (self.original_manifest, self.original_lock))

    def test_refuse_corrupted_backup(self):
        self.pair.prepare()
        (self.pair.state / "Cargo.toml.original").write_bytes(b"corrupt\n")
        before = self.snapshot()
        with self.assertRaises(helper.Refusal):
            self.pair.restore()
        self.assertEqual(self.snapshot(), before)

    def test_atomic_replace_preserves_modes(self):
        (self.motion / "Cargo.toml").chmod(0o640)
        self.pair.prepare()
        self.assertEqual((self.motion / "Cargo.toml").stat().st_mode & 0o777, 0o640)

    def test_replace_refuses_changed_input(self):
        with self.assertRaises(helper.Refusal):
            helper.replace_checked(self.motion / "Cargo.toml", b"not current", b"replacement")
        self.assertEqual((self.motion / "Cargo.toml").read_bytes(), self.original_manifest)


if __name__ == "__main__":
    unittest.main(verbosity=2)

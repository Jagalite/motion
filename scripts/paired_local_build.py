#!/usr/bin/env python3
"""Safely select the sibling Catabolic checkout. Python 3.11+, stdlib only.

No subprocesses, network, installs, builds, migrations, or data-directory access.
Run from an extracted pair; see ../LOCAL_BUILD.md. Stop concurrent source edits
and Cargo operations before use. This is a fixed-source handoff, not a generic
Cargo manifest/lock editor or a claim of application qualification.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import copy
import hashlib
import os
from pathlib import Path
import stat
import sys
import tempfile
import tomllib

ORIGINAL_MANIFEST_SHA256 = "279686b90637acb8f61a2d12e80e4ad337e7abad45f4435e0c2e810407687ca4"
ORIGINAL_LOCK_SHA256 = "948b788dc4454a4465bd0148e5c04d8e1a20ac5943507fc72714912e099c3caa"
REV = "6969f08c5d9b207afe8eec819555759be136644d"
GIT = "https://github.com/Jagalite/catabolic.git"
SOURCE = f"git+{GIT}?rev={REV}#{REV}"
PATHS = {
    "catabolic-core": "crates/catabolic-core",
    "catabolic-store": "crates/catabolic-store",
    "catabolic-runtime": "crates/catabolic-runtime",
    "fancy-regex": "vendor/fancy-regex",
}
# All manifests in Motion's workspace and the paired local dependency closure.
# A changed manifest requires review and a new handoff, not a --force option.
PAIRED_MANIFESTS = {
    "motion/crates/core/Cargo.toml": "729970cade2686f06cbb1a58044c26c67c44482c43a11e8eae0185b995141777",
    "motion/crates/motion-ui/Cargo.toml": "e93c1606cc0187a3efa6ac65851969be0b8591ac5258ca3cbe4cd6cf75ea0c31",
    "motion/crates/motion-ui-host/Cargo.toml": "65429f641b3ad3c2a1851022a59de4f77c2369667188d67ad4fe90e6605086ef",
    "motion/crates/catalog-backend/Cargo.toml": "1f924426a5a6643af2a8cfab5a294d9701f5801247598c5c2a102a4a90b94e9b",
    "catabolic/Cargo.toml": "ff8a7308ebb51b2802c452aa8b9fe5158a2bb33fe8c6cf22142d1f069c2b9aa9",
    "catabolic/crates/catabolic-core/Cargo.toml": "b66092697ff4c52ae9ea7c5e4c72eafefb559972eb7ac20ce7a97586280d350c",
    "catabolic/crates/catabolic-store/Cargo.toml": "cea870c1f540f0af79e68395d90efdb544c30966046dfbd4227124c92d1247ab",
    "catabolic/crates/catabolic-runtime/Cargo.toml": "fdc3e2e13f2a81836f4b6e1da13fba0d92c2f120fb3964e858f7325ed763a236",
    "catabolic/vendor/fancy-regex/Cargo.toml": "5296c3d4930cb7afe9d589902041dd9a0afa76f521d46e110bad42f6abc530b3",
}
STATE_NAME = ".paired-local-build"
READY = b"paired-local-build-v1\n"


class Refusal(Exception):
    """A precondition failed; do not overwrite the user's work."""


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_regular(root: Path, relative: str) -> bytes:
    path = root
    for part in Path(relative).parts:
        path = path / part
        if path.is_symlink():
            raise Refusal(f"Refusing symlink: {path}")
    if not path.is_file():
        raise Refusal(f"Missing regular file: {path}")
    return path.read_bytes()


def expect_hash(data: bytes, digest: str, label: str) -> None:
    if sha(data) != digest:
        raise Refusal(f"Unexpected {label}; preserve your changes and use a fresh source pair.")


def localized_manifest(original: bytes) -> bytes:
    expect_hash(original, ORIGINAL_MANIFEST_SHA256, "original Motion manifest")
    result = original
    for name, subpath in PATHS.items():
        old = f'{name} = {{ git = "{GIT}", rev = "{REV}" }}'.encode()
        new = f'{name} = {{ path = "../catabolic/{subpath}" }}'.encode()
        if result.splitlines().count(old) != 1:
            raise Refusal(f"Expected exactly one full dependency line for {name}.")
        result = result.replace(old, new, 1)
    return result


def expected_local_lock(original: bytes) -> dict:
    """Validate Cargo's output, never synthesize a lock to use for a build."""
    expect_hash(original, ORIGINAL_LOCK_SHA256, "original Motion lock")
    expected = copy.deepcopy(tomllib.loads(original.decode("utf-8")))
    packages = expected["package"]
    for name in PATHS:
        matches = [p for p in packages if p["name"] == name]
        if len(matches) != 1 or matches[0].pop("source", None) != SOURCE:
            raise Refusal(f"Unexpected original lock entry for {name}.")
    # Current paired source adds these edges; package versions stay unchanged.
    for name, additions in {
        "catabolic-runtime": ("serde",),
        "motion-catalog-backend": ("libc", "rusqlite"),
    }.items():
        matches = [p for p in packages if p["name"] == name]
        if len(matches) != 1:
            raise Refusal(f"Expected one original lock entry for {name}.")
        dependencies = matches[0]["dependencies"]
        if any(value in dependencies for value in additions):
            raise Refusal(f"Unexpected original dependency edges for {name}.")
        dependencies.extend(additions)
        dependencies.sort()
    return expected


def validate_local_lock(original: bytes, local: bytes) -> None:
    try:
        actual = tomllib.loads(local.decode("utf-8"))
    except (UnicodeError, tomllib.TOMLDecodeError) as error:
        raise Refusal(f"Invalid normalized Cargo.lock: {error}") from error
    if actual != expected_local_lock(original):
        raise Refusal("Normalized lock has unexpected changes. Nothing was accepted; "
                      "preserve the lock and review it. Do not delete it or run cargo update.")


def write_new(path: Path, data: bytes) -> None:
    # Exclusive creation never replaces an existing backup or guard.
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def replace_checked(path: Path, before: bytes, after: bytes) -> None:
    if path.is_symlink() or not path.is_file() or path.read_bytes() != before:
        raise Refusal(f"Changed during operation: {path}; nothing overwritten at this step.")
    mode = stat.S_IMODE(path.stat().st_mode)
    name = None
    try:
        with tempfile.NamedTemporaryFile(prefix=".paired-write-", dir=path.parent, delete=False) as stream:
            name = Path(stream.name)
            stream.write(after)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(name, mode)
        if path.is_symlink() or path.read_bytes() != before:
            raise Refusal(f"Changed during operation: {path}; nothing overwritten at this step.")
        os.replace(name, path)
    finally:
        if name is not None and name.exists():
            name.unlink()


@contextmanager
def exclusive_operation(motion: Path):
    guard = motion / ".paired-local-build.guard"
    try:
        write_new(guard, f"pid={os.getpid()}\n".encode())
    except FileExistsError as error:
        raise Refusal(f"Operation guard exists: {guard}. See LOCAL_BUILD.md before removing it.") from error
    try:
        yield
    finally:
        guard.unlink()


class Pair:
    def __init__(self, motion: Path):
        if motion.is_symlink():
            raise Refusal("The Motion directory must not be a symlink.")
        self.motion = motion.resolve(strict=True)
        if self.motion.name != "motion" or not self.motion.is_dir():
            raise Refusal("Expected the extracted motion/ directory beside catabolic/.")
        self.root = self.motion.parent
        self.state = self.motion / STATE_NAME
        if self.state.is_symlink():
            raise Refusal("Backup directory must not be a symlink.")

    def inputs(self) -> tuple[bytes, bytes]:
        return (read_regular(self.motion, "Cargo.toml"),
                read_regular(self.motion, "Cargo.lock"))

    def check_pair(self) -> None:
        for name, digest in PAIRED_MANIFESTS.items():
            expect_hash(read_regular(self.root, name), digest, name)

    def backups(self) -> tuple[bytes, bytes, bytes | None]:
        if read_regular(self.state, "READY") != READY:
            raise Refusal("Incomplete or unexpected backups; see manual recovery in LOCAL_BUILD.md.")
        manifest = read_regular(self.state, "Cargo.toml.original")
        lock = read_regular(self.state, "Cargo.lock.original")
        expect_hash(manifest, ORIGINAL_MANIFEST_SHA256, "manifest backup")
        expect_hash(lock, ORIGINAL_LOCK_SHA256, "lock backup")
        local_path = self.state / "Cargo.lock.local"
        local = read_regular(self.state, "Cargo.lock.local") if os.path.lexists(local_path) else None
        if local is not None:
            validate_local_lock(lock, local)
        return manifest, lock, local

    def inspect(self) -> str:
        manifest, lock = self.inputs()
        if not self.state.exists():
            expect_hash(manifest, ORIGINAL_MANIFEST_SHA256, "Motion manifest")
            expect_hash(lock, ORIGINAL_LOCK_SHA256, "Motion lock")
            return "original"
        original, original_lock, local_lock = self.backups()
        if manifest not in (original, localized_manifest(original)):
            raise Refusal("Motion manifest changed outside this helper; refusing to replace it.")
        if lock != original_lock and lock != local_lock:
            raise Refusal("Motion lock is not original or previously verified. "
                          "After successful offline metadata, run verify-lock; otherwise preserve and review it.")
        if manifest == original:
            if lock != original_lock:
                raise Refusal("Original manifest with a localized lock; use restore after review.")
            return "original"
        return "localized-verified" if lock == local_lock else "localized-awaiting-lock"

    def prepare(self) -> str:
        self.check_pair()
        state = self.inspect()
        if state != "original":
            return f"Already prepared: {state}; no source files changed."
        manifest, lock = self.inputs()
        if not self.state.exists():
            self.state.mkdir()  # Exclusive: never adopt unrelated existing state.
            write_new(self.state / "Cargo.toml.original", manifest)
            write_new(self.state / "Cargo.lock.original", lock)
            write_new(self.state / "READY", READY)
        if self.inputs() != (manifest, lock):
            raise Refusal("Inputs changed while backing up; source files left untouched.")
        replace_checked(self.motion / "Cargo.toml", manifest, localized_manifest(manifest))
        return "Prepared four local paths. Original lock retained; run offline metadata, then verify-lock."

    def verify_lock(self) -> str:
        self.check_pair()
        original, original_lock, recorded = self.backups()
        manifest, lock = self.inputs()
        if manifest != localized_manifest(original):
            raise Refusal("Expected the exact prepared Motion manifest.")
        validate_local_lock(original_lock, lock)
        if recorded is not None and recorded != lock:
            raise Refusal("A different normalized lock is already recorded; preserve and review both.")
        if self.inputs() != (manifest, lock):
            raise Refusal("Inputs changed during verification; nothing recorded.")
        if recorded is None:
            write_new(self.state / "Cargo.lock.local", lock)
        return ("Verified lock: same 572 packages/versions, four local sources and three required "
                "dependency edges. No build or runtime qualification implied.")

    def restore(self) -> str:
        # Restoring managed originals must remain possible after a paired source edit.
        original, original_lock, recorded = self.backups()
        manifest, lock = self.inputs()
        if manifest not in (original, localized_manifest(original)):
            raise Refusal("Unexpected Motion manifest; preserve it before manual recovery.")
        if lock != original_lock and lock != recorded:
            raise Refusal("Unrecorded Motion lock; refusing rollback over possible custom changes. "
                          "Use verify-lock only for expected Cargo output, or see manual recovery.")
        if lock != original_lock:
            replace_checked(self.motion / "Cargo.lock", lock, original_lock)
        if manifest != original:
            replace_checked(self.motion / "Cargo.toml", manifest, original)
        return "Original manifest and lock restored. Backups and verified local lock retained."


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("status", "prepare", "verify-lock", "restore"), nargs="?", default="status")
    parser.add_argument("--motion-dir", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args(argv)
    try:
        pair = Pair(args.motion_dir)
        if args.action == "status":
            pair.check_pair()
            print(f"Source state: {pair.inspect()}. Read-only check; no build performed.")
        else:
            with exclusive_operation(pair.motion):
                operation = {"prepare": pair.prepare, "verify-lock": pair.verify_lock, "restore": pair.restore}
                print(operation[args.action]())
        return 0
    except (Refusal, OSError, ValueError, KeyError) as error:
        print(f"Refused: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

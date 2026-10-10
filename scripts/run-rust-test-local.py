#!/usr/bin/env python3
"""Run identical Rust test bytes internally when an external macOS volume stalls.
Opt in with CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER pointing at this file.
"""
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile

source = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix="motion-rust-test-") as directory:
    target = Path(directory) / source.name
    data = source.read_bytes()
    target.write_bytes(data)
    target.chmod(0o700)
    digest = hashlib.sha256(data).hexdigest()
    if hashlib.sha256(target.read_bytes()).hexdigest() != digest:
        raise RuntimeError("Staged test bytes differ")
    print(f"Staged {source.name} sha256={digest}", flush=True)
    result = subprocess.run([str(target), *sys.argv[2:]], env=os.environ)
    sys.exit(result.returncode)

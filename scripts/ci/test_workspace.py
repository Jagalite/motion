"""Run every workspace unit, integration and doc test with a bounded real helper.

Usage: python3 scripts/ci/test_workspace.py [--print]
Requires the repository Rust toolchain, ffmpeg, ffprobe and Ruby (YAML/JSON stdlib). Does not install tools
or strip an already-built binary. Cargo's opt-in profile performs linker stripping.
Run from an otherwise immutable checkout; do not edit sources while tests run.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess

ROOT = Path(__file__).resolve().parents[2]
PROFILE = "native-qualification"
MAX_HELPER_BYTES = 128 * 1024 * 1024


def commands():
    return [
        ["cargo", "test", "--locked", "--profile", PROFILE, "--workspace", "--no-fail-fast", "--no-run", "--message-format=json-render-diagnostics"],
        ["cargo", "test", "--locked", "--profile", PROFILE, "--workspace", "--no-fail-fast"],
    ]


def qualification_environment(environment):
    """Bound parallel test load without reducing any target or model exploration."""
    result = dict(environment)
    result.setdefault("RUST_TEST_THREADS", "1")
    return result


def verify_helper(path):
    """Fail before test execution if the actual CARGO_BIN_EXE helper is oversized."""
    with Path(path).open("rb") as source:
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_HELPER_BYTES:
            raise RuntimeError(f"observer helper must be a nonempty regular file <= {MAX_HELPER_BYTES} bytes")
        digest = hashlib.file_digest(source, "sha256").hexdigest()
        after = os.fstat(source.fileno())
        if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
            after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns
        ):
            raise RuntimeError("observer helper changed while recording its identity")
    return {"path": str(path), "bytes": before.st_size, "sha256": digest}


def verified_test_environment(environment, helper):
    """Use only Cargo's actual verified helper, never an inherited harness path."""
    proof = verify_helper(Path(helper).resolve(strict=True))
    result = dict(environment)
    result["MOTION_TEST_OBSERVER"] = proof["path"]
    return result, proof


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--print", action="store_true", dest="dry")
    args = parser.parse_args()
    build, test = commands()
    environment = qualification_environment(os.environ)
    print("Qualification RUST_TEST_THREADS=" + environment["RUST_TEST_THREADS"], flush=True)
    if args.dry:
        for command in (build, test):
            print("+", " ".join(command))
        print("Between commands: verify the real profile helper is <= 128 MiB and record SHA-256.")
        return
    for tool in ("cargo", "ffmpeg", "ffprobe", "ruby"):
        if shutil.which(tool) is None:
            raise SystemExit(f"Required qualification tool missing: {tool}; install it through your approved toolchain setup")
    subprocess.run(["ruby", "-ryaml", "-rjson", "-e", "exit 0"], cwd=ROOT, check=True)
    print("+", " ".join(build), flush=True)
    helper = None
    with subprocess.Popen(build, cwd=ROOT, env=environment, stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="", flush=True)
                continue
            if event.get("reason") == "compiler-message":
                print(event.get("message", {}).get("rendered", ""), end="", flush=True)
            if (event.get("reason") == "compiler-artifact"
                    and event.get("target", {}).get("name") == "playscale"
                    and "bin" in event.get("target", {}).get("kind", [])
                    and not event.get("profile", {}).get("test")
                    and event.get("executable")):
                artifact = Path(event["executable"])
                if helper is not None and helper != artifact:
                    raise RuntimeError("ambiguous actual observer executable")
                helper = artifact
        if process.wait() != 0:
            raise SystemExit(process.returncode)
    if helper is None:
        raise RuntimeError("Cargo did not identify the actual observer executable")
    environment, proof = verified_test_environment(environment, helper)
    print("Bounded helper build:", json.dumps(proof, sort_keys=True), flush=True)
    print("+", " ".join(test), flush=True)
    subprocess.run(test, cwd=ROOT, env=environment, check=True)


if __name__ == "__main__":
    main()

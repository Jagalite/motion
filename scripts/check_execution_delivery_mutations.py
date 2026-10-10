#!/usr/bin/env python3
"""Check four execution/delivery regressions in a disposable core-only workspace.

Requires cached Cargo dependencies. Never edits the checkout or shares its target
directory. A compile error, timeout, or missing test is not a detected mutation.
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
HEARTBEAT = "heartbeat_boundaries_renew_only_valid_generation_leases"
STUCK = "work::tests::stuck_owner_keeps_capacity_until_exit"


def check(workspace, target, test, should_fail=False):
    command = ["cargo", "test", "--offline", "-p", "playscale-core", *target,
               test, "--", "--exact", "--nocapture"]
    env = dict(os.environ, CARGO_TARGET_DIR=str(workspace / "target"),
               CARGO_INCREMENTAL="0")
    result = subprocess.run(command, cwd=workspace, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            timeout=600)
    expected = f"test {test} ... {'FAILED' if should_fail else 'ok'}"
    if (result.returncode != (101 if should_fail else 0)
            or "running 1 test" not in result.stdout
            or expected not in result.stdout):
        raise RuntimeError(f"Unexpected result for {' '.join(command)}:\n{result.stdout}")


def replace_once(source, old, new):
    if source.count(old) != 1:
        raise RuntimeError(f"Mutation anchor must match exactly once: {old!r}")
    return source.replace(old, new, 1)


def main():
    dependency = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["dependencies"]["stateless"]
    inline = ", ".join(f"{key} = {json.dumps(value)}" for key, value in dependency.items())
    with tempfile.TemporaryDirectory(prefix="playscale-core-mutations-") as temporary:
        workspace = Path(temporary)
        shutil.copytree(ROOT / "crates/core", workspace / "crates/core")
        for name in ["Cargo.lock", "rust-toolchain.toml"]:
            shutil.copy2(ROOT / name, workspace / name)
        (workspace / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/core"]\nresolver = "2"\n'
            f"[workspace.dependencies]\nstateless = {{ {inline} }}\n"
        )
        delivery_target = ["--test", "delivery_model"]
        print("Checking unmodified delivery and work baselines", flush=True)
        check(workspace, delivery_target, HEARTBEAT)
        check(workspace, ["--lib"], STUCK)
        delivery = workspace / "crates/core/src/delivery.rs"
        work = workspace / "crates/core/src/work.rs"
        original_delivery, original_work = delivery.read_text(), work.read_text()
        # Restrict heartbeat edits to the production transition arm, not the
        # input enum or other commands that have their own lease checks.
        start = original_delivery.index("        Input::Heartbeat {", original_delivery.index("pub fn transition"))
        end = original_delivery.index("        Input::Tick {", start)
        heartbeat = original_delivery[start:end]
        mutants = [
            ("pending heartbeat replaces active playhead", delivery,
             "&& d.active == Some(*active_generation)", "&& true", delivery_target, HEARTBEAT),
            ("out-of-range heartbeat renews lease", delivery,
             "if position_ms.is_some_and(|p| !within(d.duration_ms, p)) {", "if false {", delivery_target, HEARTBEAT),
            ("expired heartbeat renews lease", delivery,
             "if !d.lease_valid(*now_ms) {", "if false {", delivery_target, HEARTBEAT),
            ("cancellation frees unconfirmed worker capacity", work,
             "effects.push(Effect::Terminate { ticket: *ticket });",
             "effects.push(Effect::Terminate { ticket: *ticket });\n                next.held.remove(ticket);",
             ["--lib"], STUCK),
        ]
        for label, path, old, new, target, test in mutants:
            delivery.write_text(original_delivery)
            work.write_text(original_work)
            if path == delivery:
                mutated = original_delivery[:start] + replace_once(heartbeat, old, new) + original_delivery[end:]
            else:
                mutated = replace_once(original_work, old, new)
            path.write_text(mutated)
            check(workspace, target, test, should_fail=True)
            print(f"Detected: {label}", flush=True)
        print("PASS: 2 baselines; 4 compiled mutations detected by test failures", flush=True)


if __name__ == "__main__":
    main()

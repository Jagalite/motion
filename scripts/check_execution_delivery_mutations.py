#!/usr/bin/env python3
"""Check ten execution/delivery regressions in a disposable core-only workspace.

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
LIVENESS = "liveness_graph_covers_stalls_duplicates_regressions_and_expiry"
ADMISSION = "admission_graph_covers_lost_ack_retirement_restart_conflict_and_rollback"
SCOPE = "delivery_admission::tests::exact_retries_replay_but_changed_requests_and_foreign_receipts_do_not"


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
        liveness_target = ["--test", "execution_deadline_model"]
        admission_target = ["--test", "delivery_admission_model"]
        print("Checking unmodified delivery, work, deadline and admission baselines", flush=True)
        check(workspace, delivery_target, HEARTBEAT)
        check(workspace, ["--lib"], STUCK)
        check(workspace, liveness_target, LIVENESS)
        check(workspace, admission_target, ADMISSION)
        check(workspace, ["--lib"], SCOPE)
        delivery = workspace / "crates/core/src/delivery.rs"
        work = workspace / "crates/core/src/work.rs"
        deadline = workspace / "crates/core/src/execution_deadline.rs"
        admission = workspace / "crates/core/src/delivery_admission.rs"
        originals = {path: path.read_text() for path in [delivery, work, deadline, admission]}
        original_delivery = originals[delivery]
        # Restrict heartbeat edits to the production transition arm, not the
        # input enum or other commands that have their own lease checks.
        start = original_delivery.index("        Input::Heartbeat {", original_delivery.index("pub fn transition"))
        end = original_delivery.index("        Input::Tick {", start)
        heartbeat = original_delivery[start:end]
        mutants = [
            ("retry creates another delivery", admission,
             "Ok(Decision::Replay {\n        delivery_id: receipt.delivery_id.clone(),\n    })",
             "Ok(Decision::Create)", admission_target, ADMISSION),
            ("changed request reuses a receipt", admission,
             "if receipt.identity.digest != request.digest {", "if false {", admission_target, ADMISSION),
            ("foreign principal receipt is accepted", admission,
             "receipt.identity.principal != request.principal ||", "false ||", ["--lib"], SCOPE),
            ("duplicate progress renews stall deadline", deadline,
             "position_ms > self.position_ms", "position_ms >= self.position_ms", liveness_target, LIVENESS),
            ("late progress revives an expired execution", deadline,
             "self.expired(elapsed_ms).is_none() &&", "true &&", liveness_target, LIVENESS),
            ("progress can outlive total deadline", deadline,
             "elapsed_ms >= self.total_ms", "false", liveness_target, LIVENESS),
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
            for original_path, original in originals.items():
                original_path.write_text(original)
            if path == delivery:
                mutated = original_delivery[:start] + replace_once(heartbeat, old, new) + original_delivery[end:]
            else:
                mutated = replace_once(originals[path], old, new)
            path.write_text(mutated)
            check(workspace, target, test, should_fail=True)
            print(f"Detected: {label}", flush=True)
        print("PASS: 5 baselines; 10 compiled mutations detected by test failures", flush=True)


if __name__ == "__main__":
    main()

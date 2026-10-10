"""Run the real-process integration suite against target/debug/playscale.

    cargo build --locked -p playscale --bin playscale
    python3 scripts/integration_checks.py --work artifacts/integration

Each check runs in its own process with a bounded duration; its output is kept
under --work and summarized in receipt.json. All checks use generated media and
disposable data directories under --work/tmp. FFmpeg/FFprobe must be on PATH.

Not included: scripts/test_demuxe_bundle.mjs needs an installed Demuxe bundle and
runs in the package workflow; scripts/test_package.py needs a built archive.
"""
import argparse
import hashlib
import json
import os
import pathlib
import signal
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
PY = [sys.executable]
CHECKS = [
    # Rust server, real HTTP and SQLite.
    ('smoke', PY + ['scripts/smoke.py'], 300),
    ('operations', PY + ['scripts/operations_smoke.py'], 300),
    ('catalog', PY + ['scripts/catalog_smoke.py'], 300),
    ('viewing', PY + ['scripts/viewing_smoke.py'], 300),
    ('processing', PY + ['scripts/processing_smoke.py'], 900),
    ('reliability', PY + ['scripts/reliability_smoke.py'], 600),
    ('video_profiles', PY + ['scripts/video_profiles_smoke.py', '--binary', 'target/debug/playscale'], 900),
    # Operator tooling.
    ('backup_tool', PY + ['scripts/test_backup.py'], 120),
    ('packaging_helpers', PY + ['scripts/test_packaging.py'], 120),
    # Browser client modules under Node.
    ('demuxe_integration', ['node', '--test', 'scripts/test_demuxe_integration.mjs'], 120),
    ('web_playback', ['node', 'scripts/test_playback.mjs'], 120),
    ('web_session', ['node', 'scripts/test_session.mjs'], 120),
    ('web_native_handoff', ['node', 'scripts/test_native_handoff.mjs'], 120),
]


def session_members(sid):
    """Live processes in session `sid`. Encoders start their own process groups
    (src/processing.rs) but stay in the session, even after reparenting."""
    members = []
    for line in subprocess.run(['ps', '-A', '-o', 'pid=,stat='], capture_output=True, text=True,
                               check=True).stdout.splitlines():
        pid, stat = line.split(None, 1)
        if stat.startswith('Z'):
            continue
        try:
            if os.getsid(int(pid)) == sid:
                members.append(int(pid))
        except (ProcessLookupError, PermissionError):
            pass
    return members


def terminate_session(process):
    """Stop everything left in the check's session; True if anything was."""
    process.poll()
    members = session_members(process.pid)
    if not members:
        process.wait()
        return False
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for pid in members:
            try:
                os.kill(pid, sig)
            except (ProcessLookupError, PermissionError):
                pass
        deadline = time.monotonic() + 10
        while members and time.monotonic() < deadline:
            time.sleep(.1)
            process.poll()  # reap the leader
            members = session_members(process.pid)
        if not members:
            break
    process.wait()
    if members:
        raise SystemExit(f'processes {members} survived SIGKILL')
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--work', type=pathlib.Path, required=True, help='new directory for logs and receipt')
    parser.add_argument('--only', action='append', help='run only the named check (repeatable)')
    args = parser.parse_args()
    work = args.work.resolve()
    work.mkdir(parents=True)
    binary = ROOT / 'target/debug/playscale'
    if not binary.is_file():
        raise SystemExit(f'{binary} is missing; run cargo build --locked -p playscale --bin playscale')
    selected = [c for c in CHECKS if not args.only or c[0] in args.only]
    unknown = set(args.only or ()) - {c[0] for c in CHECKS}
    if unknown:
        raise SystemExit(f'unknown checks: {sorted(unknown)}')
    results = []
    for name, command, limit in selected:
        log = work / f'{name}.log'
        started = time.monotonic()
        with log.open('wb') as stream:
            # Own session: a timed-out check's servers, supervisors and encoders
            # (in their own process groups) are terminated with it instead of
            # outliving it into later checks.
            # Disposable data stays under --work, not the shared system temp volume.
            temporary = work / 'tmp' / name
            temporary.mkdir(parents=True)
            process = subprocess.Popen(command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT,
                                       stdin=subprocess.DEVNULL, start_new_session=True,
                                       env=dict(os.environ, TMPDIR=str(temporary)))
            try:
                code = process.wait(timeout=limit)
            except subprocess.TimeoutExpired:
                code = 'timeout'
            finally:
                leftovers = terminate_session(process)
        if leftovers and code == 0:
            code = 'left processes running'
        results.append({'check': name, 'exit': code, 'seconds': round(time.monotonic() - started, 1)})
        print(f'{name}: {"ok" if code == 0 else f"FAILED ({code}), see {log}"}', flush=True)
    receipt = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'results': results}
    (work / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    failed = [r['check'] for r in results if r['exit'] != 0]
    if failed:
        raise SystemExit(f'failed: {", ".join(failed)}')


if __name__ == '__main__':
    main()

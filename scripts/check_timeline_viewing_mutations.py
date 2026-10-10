"""Mutation check for the timeline viewing model: each compiled regression of
crates/core/src/timeline_viewing.rs must make the Stateless model fail.

Run from the repository root: python3 scripts/check_timeline_viewing_mutations.py
Set KYOTO=/path/to/kyoto.py to submit each cargo run through that build queue.
The source file is restored after every mutation.
"""
import json, os, pathlib, subprocess, sys, types

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'crates/core/src/timeline_viewing.rs'
MUTATIONS = [
    ('sequence_gaps_accepted',
     '    if event.sequence != session.sequence + 1 {\n        return Err(Error::SequenceGap);\n    }\n', ''),
    ('override_keeps_the_manual_epoch',
     '            manual_epoch: next(view.manual_epoch)?,\n', ''),
    ('retry_ignores_content',
     '        return if p == event {', '        return if p.sequence == event.sequence || p.id == event.id {'),
    ('superseded_session_still_writes',
     '    if !authoritative(view, session) {\n        return Err(Error::Superseded);\n    }\n    if session.status.closed() {\n        return Err(Error::Closed);\n    }\n    if event.sequence <= session.sequence {',
     '    if session.status.closed() {\n        return Err(Error::Closed);\n    }\n    if event.sequence <= session.sequence {'),
]


def model():
    command = ['cargo', 'test', '-p', 'playscale-core', '--release', '--test',
               'timeline_viewing_model', '--', '--nocapture']
    kyoto = os.environ.get('KYOTO')
    if not kyoto:
        return subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    job = json.loads(subprocess.run([kyoto, 'submit', '--cwd', str(ROOT), '--', *command],
                                    check=True, capture_output=True, text=True).stdout)
    done = json.loads(subprocess.run([kyoto, 'wait', job['id']], check=True,
                                     capture_output=True, text=True).stdout)
    log = pathlib.Path(done['log']).read_text(errors='replace')
    return types.SimpleNamespace(returncode=done['exit_code'], stdout=log, stderr=log)


def main():
    original = SOURCE.read_text()
    baseline = model()
    assert baseline.returncode == 0, baseline.stdout + baseline.stderr
    results = {}
    try:
        for name, old, new in MUTATIONS:
            assert original.count(old) == 1, name
            SOURCE.write_text(original.replace(old, new))
            run = model()
            compiled = 'error[' not in run.stderr
            results[name] = 'detected' if compiled and run.returncode != 0 else (
                'did_not_compile' if not compiled else 'MISSED')
            print(name, results[name], file=sys.stderr)
    finally:
        SOURCE.write_text(original)
    print(results)
    if any(v != 'detected' for v in results.values()):
        raise SystemExit('a mutation was not detected')


if __name__ == '__main__':
    main()

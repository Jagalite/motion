"""Run one tier of the workspace tests.

fast: every test target except FULL_ONLY, plus unit and doc tests. This is
      the pull-request merge gate (ci.yml).
full: only FULL_ONLY, the long-running suites (ci-full.yml: pushes to main,
      nightly, on demand, and pull requests touching production source,
      models/tests, clients, contracts, build inputs or workflows).

Together the tiers cover `cargo test --workspace`. A target is full-only when
it explores a large Stateless graph in an unoptimized build or drives real
FFmpeg live delivery end to end. Usage: python3 scripts/ci/test_tier.py fast|full [--print]
"""
import json, subprocess, sys

FULL_ONLY = {
    # Stateless models that take minutes in a debug build.
    'access_model', 'delivery_model', 'downloads_model', 'timeline_viewing_model', 'work_model',
    # Live HLS delivery and v2 playback with real FFmpeg conversions.
    'delivery', 'v2_playback',
}


def test_targets():
    meta = json.loads(subprocess.run(
        ['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'],
        check=True, capture_output=True, text=True).stdout)
    return sorted(t['name'] for p in meta['packages'] for t in p['targets']
                  if 'test' in t['kind'])


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in ('fast', 'full'):
        raise SystemExit(__doc__)
    tier, dry = sys.argv[1], '--print' in sys.argv
    targets = test_targets()
    base = ['cargo', 'test', '--locked', '--workspace', '--no-fail-fast']
    if tier == 'fast':
        chosen = [t for t in targets if t not in FULL_ONLY]
        commands = [base + ['--lib', '--bins'] + [a for t in chosen for a in ('--test', t)],
                    ['cargo', 'test', '--locked', '--workspace', '--doc']]
    else:
        chosen = [t for t in targets if t in FULL_ONLY]
        commands = [base + [a for t in chosen for a in ('--test', t)]] if chosen else []
    failed = False
    for command in commands:
        print('+', ' '.join(command), flush=True)
        if not dry:
            failed |= subprocess.run(command).returncode != 0
    if failed:
        raise SystemExit(1)


if __name__ == '__main__':
    main()

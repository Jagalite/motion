"""Reject private source content; optionally enforce public Git identities."""
import argparse
import fnmatch
from pathlib import Path
import re
import subprocess

from package_privacy import PRIVATE_PATH, PRIVATE_HOST


# Commits already pushed to origin cannot be corrected without rewriting
# shared history. Each entry names one commit and the one role it exempts; the
# identity itself is deliberately not repeated here.
HISTORY_EXCEPTIONS = Path(__file__).with_name('publication_history_exceptions.txt')


def public_identity(email, role):
    if email.endswith('@motion.invalid') or email.endswith('@users.noreply.github.com'):
        return True
    # GitHub commits web merges as its own public web-flow identity.
    return role == 'committer' and email == 'noreply@github.com'


def history_exceptions(path=HISTORY_EXCEPTIONS):
    exceptions = set()
    if not path.exists():
        return exceptions
    for number, line in enumerate(path.read_text().splitlines(), 1):
        line = line.split('#', 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        if (len(fields) != 2 or not re.fullmatch(r'[0-9a-f]{40}', fields[0])
                or fields[1] not in ('author', 'committer')):
            raise SystemExit(f'{path.name}:{number}: expected "<40-hex commit> author|committer"')
        exceptions.add((fields[0], fields[1]))
    return exceptions


def identity_failures(root, exceptions=None):
    exceptions = history_exceptions() if exceptions is None else exceptions
    # Check the history being published (everything reachable from HEAD), not
    # unrelated local branches. Strict callers need full history for a complete scan.
    log = subprocess.check_output(['git', 'log', 'HEAD', '--format=%H%x00%ae%x00%ce'], cwd=root).decode()
    failures = []
    for line in log.splitlines():
        commit, author, committer = line.split('\0')
        for role, email in (('author', author), ('committer', committer)):
            if not public_identity(email, role) and (commit, role) not in exceptions:
                # Name the commit, never the identity: CI logs are public.
                failures.append(f'Git history: commit {commit[:12]} {role} must use a public '
                                'noreply or project-only identity')
    return failures


def source_failures(root, names):
    failures = []
    for name in names:
        path = root / name
        if not path.is_file():
            continue
        if (fnmatch.fnmatch(path.name, '.env*') and path.name != '.env.example'
                or path.name in ('config.json', 'admin-token', '.npmrc', '.netrc', '.pypirc')
                or fnmatch.fnmatch(path.name, '*.local.json')):
            failures.append(f'{name}: private configuration must not be tracked')
        data = path.read_bytes()
        if PRIVATE_PATH.search(data):
            failures.append(f'{name}: absolute developer-machine path')
        for host in PRIVATE_HOST.findall(data):
            if host not in (b'your-server.your-tailnet' + b'.ts.net',):
                failures.append(f'{name}: private tailnet hostname')
    return failures


def check(root, *, check_history=True):
    names = subprocess.check_output(['git', 'ls-files', '-z'], cwd=root).decode().split('\0')
    failures = source_failures(root, filter(None, names))
    if check_history:
        failures.extend(identity_failures(root))
    if failures:
        raise SystemExit('\n'.join(failures))
    scope = 'tracked source and Git identities' if check_history else 'tracked source'
    print(f'Publication privacy checks passed for {scope}.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-only', action='store_true',
                        help='Scan tracked source without restricting commit email identities')
    args = parser.parse_args()
    check(Path(__file__).resolve().parents[1], check_history=not args.source_only)

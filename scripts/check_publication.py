"""Reject machine-specific paths, private configuration and private Git identities."""
import fnmatch
from pathlib import Path
import re
import subprocess

from package_privacy import PRIVATE_PATH, PRIVATE_HOST


def check(root):
    failures = []
    files = subprocess.check_output(['git', 'ls-files', '-z'], cwd=root).split(b'\0')
    for raw in files:
        if not raw:
            continue
        name = raw.decode()
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
    emails = subprocess.check_output(['git', 'log', '--all', '--format=%ae%n%ce'], cwd=root).decode().splitlines()
    for email in set(emails):
        if not (email.endswith('@motion.invalid') or email.endswith('@users.noreply.github.com')):
            failures.append('Git history: use a public noreply or project-only commit identity')
    if failures:
        raise SystemExit('\n'.join(failures))
    print('Publication privacy checks passed for tracked source and Git identities.')


if __name__ == '__main__':
    check(Path(__file__).resolve().parents[1])

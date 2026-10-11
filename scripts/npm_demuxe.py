"""Fetch and verify the exact published Demuxe archive."""
import base64
import hashlib
import subprocess

from build_media_tools import digest

def npm_archive(lock, cache):
    spec = lock['demuxe']
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"demuxe-{spec['version']}.tgz"
    if not archive.exists():
        subprocess.run(['npm', 'pack', f"demuxe@{spec['version']}", '--ignore-scripts',
                        '--registry=https://registry.npmjs.org/',
                        '--pack-destination', str(cache), '--json'], check=True)
    algorithm, expected = spec['integrity'].split('-', 1)
    if algorithm != 'sha512':
        raise ValueError('Expected npm SHA-512 integrity')
    with archive.open('rb') as source:
        actual = base64.b64encode(hashlib.file_digest(source, 'sha512').digest()).decode()
    if actual != expected or digest(archive) != spec['sha256']:
        raise ValueError('Demuxe npm archive integrity mismatch')
    return archive


"""Rebuild an extracted source companion without access to the original checkout.

Build-bearing: local invocations must be submitted through the shared Kyoto queue.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import sys


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check(archive, manifest_path, work):
    manifest = json.loads(manifest_path.read_text())
    work.mkdir(parents=True, exist_ok=False)
    receipt = {'schema': 1, 'source_archive_sha256': sha(archive),
               'manifest_sha256': sha(manifest_path),
               'source_commit': manifest['source_commit'],
               'source_dirty': manifest['source_dirty'], 'status': 'failed'}
    try:
        with tarfile.open(archive) as bundle:
            members = bundle.getmembers()
            names = [m.name for m in members]
            expected = {'motion/' + name for name in manifest['source_files']}
            if len(names) != len(set(names)) or set(names) != expected:
                raise ValueError('Source archive membership does not match manifest')
            if any(not m.isfile() for m in members):
                raise ValueError('Source archive must contain only regular files')
            bundle.extractall(work, filter='data')
        root = work / 'motion'
        for name, expected_hash in manifest['source_files'].items():
            if sha(root / name) != expected_hash:
                raise ValueError(f'Source archive hash mismatch: {name}')
        # Exercise the shipped assembly recipe outside Git, including Go,
        # Electron, media tools and the player derivative. A Rust-only rebuild
        # cannot establish provenance for the installed one-app distribution.
        output = work / 'reconstructed'
        command = [sys.executable, str(root / 'scripts/package_app.py'),
                   '--source-manifest', str(manifest_path), '--output', str(output),
                   '--cache', str(work / 'inputs'), '--media-tools', str(work / 'media-tools'),
                   '--jobs', '2']
        receipt['command'] = ['python3', 'motion/scripts/package_app.py',
                              '--source-manifest', 'original/motion-package.json',
                              '--output', 'reconstructed', '--cache', 'inputs',
                              '--media-tools', 'media-tools', '--jobs', '2']
        with (work / 'build.log').open('w') as log:
            subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, check=True)
        rebuilt_path = output / 'Motion/motion-package.json'
        rebuilt = json.loads(rebuilt_path.read_text())
        for field in ('source_commit', 'source_dirty', 'source_files', 'packaging_lock_sha256'):
            if rebuilt[field] != manifest[field]:
                raise ValueError('Reconstructed provenance differs: ' + field)
        if rebuilt['demuxe']['provider_identity'] != manifest['demuxe']['provider_identity']:
            raise ValueError('Reconstructed player provider identity differs')
        receipt['reconstructed_manifest_sha256'] = sha(rebuilt_path)
        receipt['package_sha256'] = sha(output / 'Motion-macos-arm64.tar.gz')
        receipt['provider_identity'] = rebuilt['demuxe']['provider_identity']
        receipt['products'] = ['motion-server', 'motion-ui-host', 'Go CLI/TUI',
                               'Electron', 'FFmpeg/FFprobe', 'Demuxe/Shaka derivative']
        acceptance = work / 'installed'
        subprocess.run([sys.executable, str(root / 'scripts/test_package.py'),
                        str(output / 'Motion-macos-arm64.tar.gz'), '--work', str(acceptance)],
                       cwd=root, check=True)
        receipt['installed_receipt_sha256'] = sha(acceptance / 'receipt.json')
        receipt['status'] = 'passed'
    finally:
        (work / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-archive', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(check(args.source_archive.resolve(), args.manifest.resolve(), args.work.resolve()), indent=2))

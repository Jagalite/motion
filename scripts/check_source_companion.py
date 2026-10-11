"""Rebuild an extracted source companion without access to the original checkout.

Build-bearing: local invocations must be submitted through the shared Kyoto queue.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tomllib


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
        channel = tomllib.loads((root / 'rust-toolchain.toml').read_text())['toolchain']['channel']
        toolchain = ['rustup', 'run', channel]
        command = [*toolchain, 'cargo', 'build', '--release', '--locked', '--workspace',
                   '--target-dir', str(work / 'target')]
        receipt['command'] = command
        receipt['rustc'] = subprocess.check_output([*toolchain, 'rustc', '-vV'], cwd=root, text=True)
        with (work / 'build.log').open('w') as log:
            subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, check=True)
        binary = work / 'target/release/playscale'
        receipt['binary_sha256'] = sha(binary)
        receipt['binary_smoke'] = '--help'
        receipt['help_sha256'] = hashlib.sha256(
            subprocess.check_output([str(binary), '--help'], cwd=root)).hexdigest()
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

"""Build a relocatable Motion package with npm Demuxe and standalone media tools.

The first supported target is native macOS arm64. Nothing is published.
"""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile

from check_publication import source_failures
from build_media_tools import assert_portable, build, digest, download
from package_privacy import assert_private_paths_absent, public_tar_member

ROOT = Path(__file__).resolve().parents[1]


def source_inventory():
    """Capture the complete checkout, including new nonignored build inputs.

    Git's ignore rules exclude targets, downloaded assets and local credentials.
    Fail on missing files and symlinks instead of silently omitting inputs or
    capturing bytes outside the checkout. This also includes every workspace
    member, embedded asset, fixture, lockfile and build/verification recipe.
    """
    names = subprocess.check_output(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'],
        cwd=ROOT).decode().split('\0')
    result = {}
    for name in sorted(set(names) - {''}):
        path = ROOT / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f'Source input is missing or not a regular file: {name}')
        result[name] = digest(path)
    failures = source_failures(ROOT, result)
    if failures:
        raise ValueError('Source publication privacy check failed: ' + '; '.join(failures))
    return result


def write_source_archive(destination, inputs):
    # Recheck membership as well as hashes: additions and removals matter too.
    if inputs != source_inventory():
        raise ValueError('Application source changed after compilation')
    with tarfile.open(destination, 'w:gz') as archive:
        for name, expected in inputs.items():
            contents = (ROOT / name).read_bytes()
            if hashlib.sha256(contents).hexdigest() != expected:
                raise ValueError(f'Application source changed after compilation: {name}')
            member = tarfile.TarInfo('motion/' + name)
            member.size = len(contents)
            member.mode = 0o755 if (ROOT / name).stat().st_mode & 0o111 else 0o644
            archive.addfile(public_tar_member(member), io.BytesIO(contents))


def npm_archive(lock, cache):
    spec = lock['demuxe']
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


def assemble(output, cache, tools, jobs):
    inputs = source_inventory()
    lock = json.loads((ROOT / 'packaging.lock.json').read_text())
    if (platform.system(), platform.machine()) != ('Darwin', 'arm64'):
        raise ValueError('This builder currently supports native macOS arm64 only')
    if output.exists():
        raise ValueError('Output already exists; choose a new destination')
    output.parent.mkdir(parents=True, exist_ok=True)
    cache.mkdir(parents=True, exist_ok=True)
    archive = npm_archive(lock, cache)
    if not tools.exists():
        build(tools, cache, jobs)
    if not (tools / 'build-receipt.json').is_file():
        raise ValueError('Incomplete media-tool build; use a fresh --media-tools directory')
    receipt = json.loads((tools / 'build-receipt.json').read_text())
    if receipt.get('schema') != 2 or not receipt.get('portable_paths'):
        raise ValueError('Rebuild media tools with portable paths in a fresh --media-tools directory')
    if receipt['target'] != lock['target'] or receipt['sources'] != {key: lock[key] for key in ('ffmpeg', 'x264')}:
        raise ValueError('Media tools do not match packaging.lock.json')
    for name, expected in receipt['binaries'].items():
        binary = tools / 'install/bin' / name
        if digest(binary) != expected:
            raise ValueError(f'Media binary changed: {name}')
        assert_portable(binary)
    if set(receipt['binaries']) != {'ffmpeg', 'ffprobe'}:
        raise ValueError('Media receipt must contain exactly ffmpeg and ffprobe')
    if inputs != source_inventory():
        raise ValueError('Application source changed during dependency preparation')
    rust_env = {**os.environ}
    # Override inherited compiler flags so all local paths use public prefixes.
    rust_env.pop('RUSTFLAGS', None)
    rust_env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join([
        f'--remap-path-prefix={Path.home()}=/build/cargo',
        f'--remap-path-prefix={ROOT}=/build/motion',
    ])
    subprocess.run(['cargo', 'build', '--release', '--locked', '--target-dir', str(ROOT / 'target'),
                    '--jobs', str(jobs)], cwd=ROOT, env=rust_env, check=True)
    if inputs != source_inventory():
        raise ValueError('Application source changed during compilation; retry the build')
    binary = ROOT / 'target/release/playscale'
    assert_portable(binary)
    # Use a sibling staging directory. Publish only after all inputs and inventories
    # succeed; failed builds leave the requested output absent and can be retried.
    with tempfile.TemporaryDirectory(prefix='.motion-build-', dir=output.parent) as temporary:
        stage = Path(temporary) / 'release'
        app = stage / 'Motion'
        app.mkdir(parents=True)
        shutil.copy2(binary, app / 'motion')
        (app / 'tools').mkdir()
        for name in ('ffmpeg', 'ffprobe'):
            shutil.copy2(tools / 'install/bin' / name, app / 'tools' / name)
        subprocess.run([sys.executable, str(ROOT / 'scripts/install_demuxe.py'), str(archive),
                        '--output', str(app / 'assets/demuxe')], check=True)
        deployment = json.loads((app / 'assets/demuxe/playscale-package.json').read_text())
        subprocess.run(['node', '--test', str(ROOT / 'scripts/test_demuxe_bundle.mjs')],
                       env={**os.environ, 'DEMUXE_DIR': str(app / 'assets/demuxe')}, check=True)
        subprocess.run([sys.executable, str(ROOT / 'scripts/third_party_notices.py'),
                        '--target', lock['target'], '--output', str(app / 'THIRD_PARTY_NOTICES.txt')], check=True)
        for name in ('LICENSE', 'THIRD_PARTY.md', 'config.example.json'):
            shutil.copy2(ROOT / name, app / name)
        notices = app / 'licenses/media-tools'
        notices.mkdir(parents=True)
        for name in ('ffmpeg', 'x264'):
            source, = (tools / name).iterdir()
            for path in source.iterdir():
                if path.is_file() and path.name.startswith(('COPYING', 'LICENSE')):
                    shutil.copy2(path, notices / f'{name}-{path.name}')
        launcher = app / 'Motion.command'
        launcher.write_text('#!/bin/sh\ncd -- "$(/usr/bin/dirname -- "$0")" || exit 1\nexec ./motion --open-browser "$@"\n')
        launcher.chmod(0o755)
        (app / 'README.txt').write_text('''Motion for macOS Apple Silicon

Extract the whole Motion folder. Double-click Motion.command, or run:
  ./motion --library /absolute/path/to/media
Open http://127.0.0.1:8787. Stop with Ctrl+C in the terminal.
Rust, Node, npm, Python, and Homebrew are not needed to run this package.

Data and the admin-token file: ~/Library/Application Support/Motion
Use --data-dir to override. The server prints the token file location at startup;
paste its contents into Library administration to add folders in the browser.
Keep your data directory when replacing or moving this application folder.
For local settings, copy config.example.json to config.local.json and run:
  ./motion --config config.local.json
JSON configuration is supported; .env files are not loaded automatically.

Demuxe and FFmpeg/FFprobe are included with their original licenses. Demuxe 1.0.0
does not provide the later shared-runtime cache API; playback uses its published
per-player resource ownership. See motion-package.json for exact input hashes.
The sibling sources directory supplies matching media component sources and the
build recipe. Redistribute the source companion alongside the application archive.

This local build is not Developer ID signed or notarized.
''')
        sources = stage / 'sources'
        sources.mkdir()
        write_source_archive(sources / 'motion-source.tar.gz', inputs)
        for spec in [lock['ffmpeg'], lock['x264'], *lock['demuxe_sources']]:
            shutil.copy2(download(spec, cache), sources / spec['archive'])
        (sources / 'scripts').mkdir()
        for script in ('build_media_tools.py', 'package_app.py', 'package_privacy.py', 'install_demuxe.py'):
            shutil.copy2(ROOT / 'scripts' / script, sources / 'scripts' / script)
        shutil.copy2(ROOT / 'packaging.lock.json', sources / 'packaging.lock.json')
        shutil.copy2(tools / 'build-receipt.json', sources / 'media-build-receipt.json')
        (sources / 'README.txt').write_text('''Corresponding application and dependency sources for this Motion build.
Extract motion-source.tar.gz and, from its motion directory, run:
  cargo build --release --locked --workspace
The pinned Rust toolchain and Cargo dependencies must be available (Cargo may
fetch dependencies). No original checkout or Git metadata is needed to rebuild.
The complete checkout includes terminal/desktop sources and their lockfiles;
those clients are not yet installed by this server package.
FFmpeg and x264 are unmodified pinned source archives. media-build-receipt.json
records exact configure/make commands and compiler/environment. On macOS arm64,
from this sources folder run:
  python3 scripts/build_media_tools.py --cache . --output build
Demuxe source companions are published by its upstream release and checksum-pinned.
Motion application code is MIT; dependency source archives retain their licenses.
''')
        manifest = {'schema': 1, 'name': 'Motion', 'target': lock['target'],
                    'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                    'source_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
                    'source_files': inputs,
                    'rustc': subprocess.check_output(['rustc', '-vV'], text=True),
                    'packaging_lock_sha256': digest(ROOT / 'packaging.lock.json'),
                    'demuxe': {**lock['demuxe'], 'deployment': deployment['deployment'], 'shared_runtime': False},
                    'media_tools': receipt,
                    'files': {str(p.relative_to(app)): digest(p) for p in sorted(app.rglob('*')) if p.is_file()},
                    'source_companion': {str(p.relative_to(sources)): digest(p) for p in sorted(sources.rglob('*')) if p.is_file()}}
        (app / 'motion-package.json').write_text(json.dumps(manifest, indent=2) + '\n')
        assert_private_paths_absent(app)
        with tarfile.open(stage / 'Motion-macos-arm64.tar.gz', 'w:gz') as bundle:
            bundle.add(app, arcname='Motion', filter=public_tar_member)
        # Source archives are already compressed; avoid recompressing a gigabyte.
        with tarfile.open(stage / 'Motion-sources.tar', 'w') as bundle:
            bundle.add(sources, arcname='sources', filter=public_tar_member)
        (stage / 'SHA256SUMS').write_text(''.join(f'{digest(p)}  {p.name}\n' for p in sorted(stage.glob('*.tar*'))))
        stage.rename(output)
    print(f'Built {output / "Motion-macos-arm64.tar.gz"}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cache', type=Path, default=ROOT / 'artifacts/package-inputs')
    parser.add_argument('--media-tools', type=Path, default=ROOT / 'artifacts/media-tools-portable-arm64')
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    assemble(args.output.resolve(), args.cache.resolve(), args.media_tools.resolve(), max(1, args.jobs))

"""Build a relocatable Motion package with npm Demuxe and standalone media tools.

The first supported target is native macOS arm64. Nothing is published.
"""
import argparse
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
import tomllib

from check_publication import source_failures
from build_media_tools import assert_portable, build, digest, download
from package_desktop import assemble_shell, sign_shell, go_notices
from build_motion_provider import build as build_provider, SHAKA
from package_privacy import assert_private_paths_absent, public_tar_member

from npm_demuxe import npm_archive

ROOT = Path(__file__).resolve().parents[1]


def source_inventory(provenance=None):
    """Capture the complete checkout, including new nonignored build inputs.

    Git's ignore rules exclude targets, downloaded assets and local credentials.
    Fail on missing files and symlinks instead of silently omitting inputs or
    capturing bytes outside the checkout. This also includes every workspace
    member, embedded asset, fixture, lockfile and build/verification recipe.
    """
    if provenance is None:
        names = subprocess.check_output(
            ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'],
            cwd=ROOT).decode().split('\0')
    else:
        expected = provenance['source_files']
        names = []
        for directory, folders, files in os.walk(ROOT):
            folders[:] = [name for name in folders if name not in
                          {'.git', 'target', 'artifacts', 'node_modules', '__pycache__'}]
            names.extend(str((Path(directory) / name).relative_to(ROOT)) for name in files)
        if set(names) != set(expected):
            raise ValueError('Extracted source membership differs from its provenance manifest')
    result = {}
    for name in sorted(set(names) - {''}):
        if Path(name).is_absolute() or '..' in Path(name).parts:
            raise ValueError('Unsafe source input name')
        path = ROOT / name
        if not path.resolve().is_relative_to(ROOT.resolve()) or path.is_symlink() or not path.is_file():
            raise ValueError(f'Source input is missing or not a regular file: {name}')
        result[name] = digest(path)
    failures = source_failures(ROOT, result)
    if failures:
        raise ValueError('Source publication privacy check failed: ' + '; '.join(failures))
    if provenance is not None and result != provenance['source_files']:
        raise ValueError('Extracted source hash mismatch')
    return result


def write_source_archive(destination, inputs, provenance=None):
    # Recheck membership as well as hashes: additions and removals matter too.
    if inputs != source_inventory(provenance):
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


def assemble(output, cache, tools, jobs, provider=None, source_manifest=None):
    provenance = json.loads(source_manifest.read_text()) if source_manifest else None
    inputs = source_inventory(provenance)
    lock = json.loads((ROOT / 'packaging.lock.json').read_text())
    if (platform.system(), platform.machine()) != ('Darwin', 'arm64'):
        raise ValueError('This builder currently supports native macOS arm64 only')
    if output.exists():
        raise ValueError('Output already exists; choose a new destination')
    output.parent.mkdir(parents=True, exist_ok=True)
    cache.mkdir(parents=True, exist_ok=True)
    core_archive = npm_archive(lock, cache)
    if provider is None:
        provider = output.parent / (output.name + '-provider')
        build_provider(provider, cache, output.parent / (output.name + '-provider-source'))
    deployment = json.loads((provider / 'playscale-package.json').read_text())
    expected_inputs = {'core': lock['demuxe']['sha256'], 'shaka_source': SHAKA['sha256'],
                       'patch': digest(ROOT / 'vendor/shaka/motion-paced.patch'),
                       'builder': digest(ROOT / 'scripts/build_motion_provider.py')}
    if deployment.get('inputs') != expected_inputs or deployment.get('deployment') != 'motion-paced-derivative':
        raise ValueError('Player deployment does not match the source inputs')
    actual_files = {str(p.relative_to(provider)) for p in provider.rglob('*') if p.is_file()} - {'playscale-package.json'}
    if actual_files != set(deployment['files']):
        raise ValueError('Player deployment inventory is incomplete')
    for name, expected in deployment['files'].items():
        path = provider / name
        if path.is_symlink() or not path.resolve().is_relative_to(provider.resolve()) or digest(path) != expected:
            raise ValueError('Player deployment file changed: ' + name)
    subprocess.run(['node', '--test', str(ROOT / 'scripts/test_demuxe_bundle.mjs')],
                   env={**os.environ, 'DEMUXE_DIR': str(provider), 'DEMUXE_RUNTIME_OWNERSHIP': 'per-player'}, check=True)
    electron_archive = download(lock['electron'], cache)
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
    if inputs != source_inventory(provenance):
        raise ValueError('Application source changed during dependency preparation')
    channel = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel']
    compiler = subprocess.check_output(
        ['rustup', 'which', '--toolchain', channel, 'rustc'], cwd=ROOT, text=True).strip()
    rust_env = {**os.environ, 'RUSTC': compiler}
    # Override inherited compiler flags so all local paths use public prefixes.
    rust_env.pop('RUSTFLAGS', None)
    rust_env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join([
        f'--remap-path-prefix={Path.home()}=/build/cargo',
        f'--remap-path-prefix={ROOT}=/build/motion',
    ])
    subprocess.run(['rustup', 'run', channel, 'cargo', 'build', '--release', '--locked', '--workspace', '--target-dir', str(ROOT / 'target'),
                    '--jobs', str(jobs)], cwd=ROOT, env=rust_env, check=True)
    if inputs != source_inventory(provenance):
        raise ValueError('Application source changed during compilation; retry the build')
    binary = ROOT / 'target/release/playscale'
    helper = ROOT / 'target/release/motion-ui-host'
    terminal = ROOT / 'target/release/motion'
    subprocess.run(['go', 'build', '-mod=readonly', '-trimpath', '-buildvcs=false', '-ldflags=-buildid=',
                    '-o', str(terminal), './cmd/motion'], cwd=ROOT / 'apps/terminal', check=True)
    for executable in (binary, helper, terminal):
        assert_portable(executable)
    if inputs != source_inventory(provenance):
        raise ValueError('Application source changed during client compilation')
    # Use a sibling staging directory. Publish only after all inputs and inventories
    # succeed; failed builds leave the requested output absent and can be retried.
    with tempfile.TemporaryDirectory(prefix='.motion-build-', dir=output.parent) as temporary:
        stage = Path(temporary) / 'release'
        app = stage / 'Motion'
        app.mkdir(parents=True)
        electron = Path(temporary) / 'electron'
        # ditto preserves the framework symlinks and executable modes from the
        # checksum-verified official zip. No package install scripts are run.
        subprocess.run(['/usr/bin/ditto', '-x', '-k', str(electron_archive), str(electron)], check=True)
        resources = assemble_shell(app, electron, binary, helper, terminal, tools / 'install/bin', provider)
        subprocess.run([sys.executable, str(ROOT / 'scripts/third_party_notices.py'),
                        '--target', lock['target'], '--output', str(app / 'THIRD_PARTY_NOTICES.txt')], check=True)
        for name in ('LICENSE', 'THIRD_PARTY.md', 'config.example.json'):
            shutil.copy2(ROOT / name, app / name)
        notices = resources / 'licenses/media-tools'
        notices.mkdir(parents=True)
        for name in ('ffmpeg', 'x264'):
            source, = (tools / name).iterdir()
            for path in source.iterdir():
                if path.is_file() and path.name.startswith(('COPYING', 'LICENSE')):
                    shutil.copy2(path, notices / f'{name}-{path.name}')
        for name in ('LICENSE', 'THIRD_PARTY.md', 'THIRD_PARTY_NOTICES.txt'):
            shutil.copy2(app / name, resources / name)
        go_notices(resources / 'licenses/go')
        sign_shell(app)
        (app / 'README.txt').write_text("""Motion for macOS Apple Silicon

Open Motion.app and choose Start local server, or attach to an existing server.
The app includes Electron, the Go/TUI client, Rust server, offline helper,
Topcoat, the exact paced player deployment, and FFmpeg/FFprobe.
No developer runtime or package manager is needed to run it.

Terminal usage: ./motion --help; ./motion tui; ./motion serve [server flags].
The native app owns only the server it starts. Remote disconnect never stops a
remote server. Keep your data and settings when replacing this application.
See motion-package.json for exact input and signed-file hashes.

This build is locally ad-hoc signed, not Developer ID signed or notarized.
Redistribute the source companion alongside the application archive.
""")
        sources = stage / 'sources'
        sources.mkdir()
        write_source_archive(sources / 'motion-source.tar.gz', inputs, provenance)
        shutil.copy2(core_archive, sources / lock['demuxe']['archive'])
        for spec in [lock['ffmpeg'], lock['x264'], SHAKA]:
            shutil.copy2(download(spec, cache), sources / spec['archive'])
        (sources / 'scripts').mkdir()
        for script in ('build_media_tools.py', 'package_privacy.py'):
            shutil.copy2(ROOT / 'scripts' / script, sources / 'scripts' / script)
        shutil.copy2(ROOT / 'packaging.lock.json', sources / 'packaging.lock.json')
        shutil.copy2(tools / 'build-receipt.json', sources / 'media-build-receipt.json')
        (sources / 'README.txt').write_text('''Corresponding application and dependency sources for this Motion build.
Extract motion-source.tar.gz and, from its motion directory, run:
  cargo build --release --locked --workspace
The pinned Rust toolchain and Cargo dependencies must be available (Cargo may
fetch dependencies). No original checkout or Git metadata is needed to rebuild.
The complete checkout includes terminal/desktop sources and their lockfiles;
both clients are installed in Motion.app. Build the Go client with go build
-trimpath -buildvcs=false ./cmd/motion from apps/terminal. Electron is the exact
checksum-pinned runtime in packaging.lock.json. The published npm Demuxe archive
is included beside this README and pinned in packaging.lock.json.
Run scripts/build_motion_provider.py to reconstruct the derivative player.
To assemble the complete package outside Git, run scripts/package_app.py with
--source-manifest /path/to/original/Motion/motion-package.json and the normal
--output, --cache and --media-tools options. Every source byte and membership
is verified against that original manifest; generated target/artifacts caches
are excluded. Keep the original manifest outside the extracted source tree.
FFmpeg and x264 are unmodified pinned source archives. media-build-receipt.json
records exact configure/make commands and compiler/environment. On macOS arm64,
from this sources folder run:
  python3 scripts/build_media_tools.py --cache . --output build
The Demuxe input is the integrity-verified npm release; Motion applies its
provider configuration and paced-HLS adapter changes during assembly.
The Shaka source archive and Motion patch reproduce its derivative assets.
Motion application code is MIT; dependency source archives retain their licenses.
''')
        manifest = {'schema': 1, 'name': 'Motion', 'target': lock['target'],
                    'source_commit': provenance['source_commit'] if provenance else subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                    'source_dirty': provenance['source_dirty'] if provenance else bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
                    'source_files': inputs,
                    'rustc': subprocess.check_output([compiler, '-vV'], text=True),
                    'packaging_lock_sha256': digest(ROOT / 'packaging.lock.json'),
                    'demuxe': {**lock['demuxe'], 'deployment': deployment['deployment'], 'shared_runtime': False, 'provider_identity': deployment['provider_identity'],
                               'receipt_sha256': digest(provider / 'playscale-package.json')},
                    'media_tools': receipt,
                    'electron': lock['electron'],
                    'go': subprocess.check_output(['go', 'version'], text=True).strip(),
                    'signing': 'ad-hoc; not Developer ID or notarized',
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
    parser.add_argument('--source-manifest', type=Path, help='Original package manifest for verified reconstruction outside Git')
    parser.add_argument('--provider', type=Path, help='Reuse an exact-input verified derivative deployment')
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    assemble(args.output.resolve(), args.cache.resolve(), args.media_tools.resolve(), max(1, args.jobs), args.provider.resolve() if args.provider else None, args.source_manifest)

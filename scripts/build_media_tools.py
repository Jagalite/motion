"""Build standalone macOS arm64 FFmpeg/FFprobe with pinned x264 sources."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def download(spec, cache):
    path = cache / spec['archive']
    cache.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        partial = path.with_suffix(path.suffix + '.partial')
        subprocess.run(['curl', '--fail', '--location', '--retry', '3',
                        '--output', str(partial), spec['url']], check=True)
        if digest(partial) != spec['sha256']:
            raise ValueError(f'Archive checksum mismatch: {path.name}')
        partial.rename(path)
    if digest(path) != spec['sha256']:
        raise ValueError(f'Archive checksum mismatch: {path.name}')
    return path


def assert_portable(binary):
    dependencies = subprocess.check_output(['/usr/bin/otool', '-L', str(binary)], text=True)
    for line in dependencies.splitlines()[1:]:
        name = line.strip().split(' (', 1)[0]
        if not name.startswith(('/usr/lib/', '/System/Library/')):
            raise ValueError(f'{binary.name} depends on a non-system library: {name}')


def build(output, cache, jobs):
    if (platform.system(), platform.machine()) != ('Darwin', 'arm64'):
        raise ValueError('Initial package builder supports native macOS arm64 only')
    lock = json.loads((ROOT / 'packaging.lock.json').read_text())
    output.mkdir(parents=True, exist_ok=False)
    prefix = output / 'install'
    # Both source directories are two levels beneath the build root. Relative
    # configure arguments also keep FFmpeg's embedded configuration portable.
    relative_prefix = Path('../../install')
    env = {**os.environ, 'MACOSX_DEPLOYMENT_TARGET': '12.0',
           'PKG_CONFIG_PATH': str(relative_prefix / 'lib/pkgconfig'),
           'PKG_CONFIG_LIBDIR': str(relative_prefix / 'lib/pkgconfig')}
    commands = []

    def run(command, cwd):
        commands.append({'cwd': str(cwd.relative_to(output)), 'argv': command})
        subprocess.run(command, cwd=cwd, env=env, check=True)

    for name in ('x264', 'ffmpeg'):
        archive = download(lock[name], cache)
        dest = output / name
        dest.mkdir()
        with tarfile.open(archive) as source:
            source.extractall(dest, filter='data')
        source, = dest.iterdir()
        if name == 'x264':
            options = ['--enable-static', '--disable-cli', '--disable-opencl']
        else:
            options = ['--disable-autodetect', '--disable-shared', '--enable-static',
                       '--disable-doc', '--disable-debug', '--disable-ffplay',
                       '--disable-network', '--enable-gpl', '--enable-libx264',
                       '--enable-videotoolbox', '--enable-audiotoolbox', '--enable-zlib',
                       '--pkg-config-flags=--static', '--cc=/usr/bin/clang',
                       f'--extra-cflags=-I{relative_prefix / "include"}',
                       f'--extra-ldflags=-L{relative_prefix / "lib"}']
        run(['./configure', f'--prefix={relative_prefix}', *options], source)
        run(['make', f'-j{jobs}'], source)
        run(['make', 'install'], source)
    for name in ('ffmpeg', 'ffprobe'):
        assert_portable(prefix / 'bin' / name)
        subprocess.run([str(prefix / 'bin' / name), '-version'], check=True)
    receipt = {'schema': 2, 'portable_paths': True, 'target': lock['target'], 'sources': {
        name: lock[name] for name in ('ffmpeg', 'x264')}, 'commands': commands,
        'environment': {key: env[key] for key in ('MACOSX_DEPLOYMENT_TARGET', 'PKG_CONFIG_PATH', 'PKG_CONFIG_LIBDIR')},
        'compiler': subprocess.check_output(['/usr/bin/clang', '--version'], text=True),
        'binaries': {name: digest(prefix / 'bin' / name) for name in ('ffmpeg', 'ffprobe')}}
    pending = output / '.build-receipt.json.partial'
    pending.write_text(json.dumps(receipt, indent=2) + '\n')
    pending.rename(output / 'build-receipt.json')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cache', type=Path, default=ROOT / 'artifacts/package-inputs')
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    build(args.output.resolve(), args.cache.resolve(), max(1, args.jobs))

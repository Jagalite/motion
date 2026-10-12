"""Build standalone macOS arm64 FFmpeg/FFprobe from pinned sources.

Statically links x264, zimg (zscale for HDR tone mapping) and libass with
FreeType/FriBidi/HarfBuzz (text subtitle rendering)."""
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


# Filters Motion's transform routes require. zscale linearizes PQ/HLG input for
# tonemap; subtitles/ass render text subtitles through libass; overlay burns in
# bitmap subtitles. Their presence is a build requirement, not a runtime guess.
REQUIRED_FILTERS = ('zscale', 'tonemap', 'subtitles', 'ass', 'overlay')


def media_sources(lock):
    """Every pinned media source, in build order (host build tools first)."""
    return (*lock['media_build_tools'], *lock['media_libraries'], 'ffmpeg')


def shipped_sources(lock):
    """Sources linked into the packaged FFmpeg/FFprobe executables."""
    return (*lock['media_libraries'], 'ffmpeg')


def filters(binary):
    listing = subprocess.check_output([str(binary), '-hide_banner', '-filters'], text=True)
    # Rows are '<flags> <name> <pads> <description>'; skip the legend.
    return {row.split()[1] for row in listing.splitlines()
            if len(row.split()) > 2 and '->' in row.split()[2]}


def assert_build_paths_absent(binary, output):
    if str(output).encode() in binary.read_bytes():
        raise ValueError(f'{binary.name} embeds the local build directory')


def build(output, cache, jobs):
    if (platform.system(), platform.machine()) != ('Darwin', 'arm64'):
        raise ValueError('Initial package builder supports native macOS arm64 only')
    lock = json.loads((ROOT / 'packaging.lock.json').read_text())
    output.mkdir(parents=True, exist_ok=False)
    prefix = output / 'install'
    # Pinned autotools bootstrap zimg's autogen.sh. They are host build inputs
    # and are not shipped; GNU libtool is prefixed so Apple's libtool stays usable.
    tools = output / 'build-tools'
    # All source directories are two levels beneath the build root. Relative
    # configure arguments also keep FFmpeg's embedded configuration portable.
    # Autoconf/CMake libraries require an absolute prefix; their paths stay in
    # build-only pkg-config files and are checked absent from the executables.
    relative_prefix = Path('../../install')
    env = {**os.environ, 'MACOSX_DEPLOYMENT_TARGET': '12.0',
           'PATH': f'{tools / "bin"}:{os.environ["PATH"]}', 'LIBTOOLIZE': 'glibtoolize',
           'PKG_CONFIG_PATH': str(relative_prefix / 'lib/pkgconfig'),
           'PKG_CONFIG_LIBDIR': str(relative_prefix / 'lib/pkgconfig')}
    commands = []

    def run(command, cwd):
        # Receipts name the build root portably; the build itself uses real paths.
        commands.append({'cwd': str(cwd.relative_to(output)),
                         'argv': [part.replace(str(output), '${BUILD}') for part in command]})
        subprocess.run(command, cwd=cwd, env=env, check=True)

    compilers = ['CC=/usr/bin/clang', 'CXX=/usr/bin/clang++']
    static = ['--disable-shared', '--enable-static', '--disable-dependency-tracking']
    configure = {
        'm4': [f'--prefix={tools}'],
        'autoconf': [f'--prefix={tools}'],
        'automake': [f'--prefix={tools}'],
        'libtool': [f'--prefix={tools}', '--program-prefix=g', '--disable-ltdl-install'],
        'zimg': [f'--prefix={prefix}', *static, *compilers],
        'freetype': [f'--prefix={prefix}', *static, *compilers, '--without-zlib', '--without-bzip2',
                     '--without-png', '--without-harfbuzz', '--without-brotli'],
        'fribidi': [f'--prefix={prefix}', *static, *compilers],
        'libass': [f'--prefix={prefix}', *static, *compilers, '--disable-fontconfig',
                   '--disable-libunibreak'],
        'x264': [f'--prefix={relative_prefix}', '--enable-static', '--disable-cli', '--disable-opencl'],
        'ffmpeg': [f'--prefix={relative_prefix}', '--disable-autodetect', '--disable-shared', '--enable-static',
                   '--disable-doc', '--disable-debug', '--disable-ffplay',
                   '--disable-network', '--enable-gpl', '--enable-libx264',
                   '--enable-libzimg', '--enable-libass',
                   '--enable-videotoolbox', '--enable-audiotoolbox', '--enable-zlib',
                   '--pkg-config-flags=--static', '--cc=/usr/bin/clang',
                   f'--extra-cflags=-I{relative_prefix / "include"}',
                   f'--extra-ldflags=-L{relative_prefix / "lib"}'],
    }

    for name in media_sources(lock):
        archive = download(lock[name], cache)
        dest = output / name
        dest.mkdir()
        with tarfile.open(archive) as source:
            source.extractall(dest, filter='data')
        source, = dest.iterdir()
        if name == 'harfbuzz':
            # HarfBuzz's CMake build needs no Meson; libass uses only its core shaper.
            run(['cmake', '-S', '.', '-B', 'build', '-G', 'Unix Makefiles',
                 '-DCMAKE_BUILD_TYPE=Release', f'-DCMAKE_INSTALL_PREFIX={prefix}',
                 '-DCMAKE_INSTALL_LIBDIR=lib', '-DCMAKE_OSX_ARCHITECTURES=arm64',
                 '-DCMAKE_OSX_DEPLOYMENT_TARGET=12.0', '-DCMAKE_C_COMPILER=/usr/bin/clang',
                 '-DCMAKE_CXX_COMPILER=/usr/bin/clang++', '-DBUILD_SHARED_LIBS=OFF',
                 '-DHB_HAVE_CORETEXT=OFF', '-DHB_BUILD_SUBSET=OFF', '-DHB_BUILD_RASTER=OFF',
                 '-DHB_BUILD_VECTOR=OFF', '-DHB_BUILD_GPU=OFF'], source)
            run(['cmake', '--build', 'build', '--parallel', str(jobs)], source)
            run(['cmake', '--install', 'build'], source)
            continue
        if name == 'zimg':
            run(['./autogen.sh'], source)
        run(['./configure', *configure[name]], source)
        # FriBidi's CLI man page needs help2man; only its library is linked.
        targets = ['SUBDIRS=gen.tab lib'] if name == 'fribidi' else []
        run(['make', f'-j{jobs}', *targets], source)
        run(['make', 'install', *targets], source)
    for name in ('ffmpeg', 'ffprobe'):
        assert_portable(prefix / 'bin' / name)
        assert_build_paths_absent(prefix / 'bin' / name, output)
        subprocess.run([str(prefix / 'bin' / name), '-version'], check=True)
    available = filters(prefix / 'bin/ffmpeg')
    missing = [name for name in REQUIRED_FILTERS if name not in available]
    if missing:
        raise ValueError(f'FFmpeg lacks required filters: {", ".join(missing)}')
    receipt = {'schema': 3, 'portable_paths': True, 'target': lock['target'], 'sources': {
        name: lock[name] for name in media_sources(lock)}, 'commands': commands,
        'environment': {key: env[key] for key in ('MACOSX_DEPLOYMENT_TARGET', 'LIBTOOLIZE', 'PKG_CONFIG_PATH', 'PKG_CONFIG_LIBDIR')},
        'compiler': subprocess.check_output(['/usr/bin/clang', '--version'], text=True),
        'cmake': subprocess.check_output(['cmake', '--version'], text=True).splitlines()[0],
        'filters': list(REQUIRED_FILTERS),
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

"""Privacy checks for distributable files and archive metadata."""
import re
import mmap
from pathlib import Path

# Absolute developer-machine paths are never needed to run a release.
PRIVATE_PATH = re.compile(rb'/(?:Users|home|Volumes)/[^/\s\x00"\x27<>]+')
PRIVATE_HOST = re.compile(rb'[a-zA-Z0-9.-]+\.ts\.net')


def private_content(data):
    for match in PRIVATE_PATH.finditer(data):
        prefix = data[max(0, match.start() - 32):match.start()]
        # Official Chromium sandbox rules name macOS system volumes. A later
        # user-home component is still scanned independently and rejected.
        if match.group() in (b'/' + b'Volumes/Data', b'/' + b'Volumes/Preboot') and prefix.endswith(b'/System'):
            continue
        # These exact browser/provider URL literals are not filesystem paths.
        if match.group() == b'/' + b'home/privacy' and prefix.endswith(b'https://www.quad9.net'):
            continue
        if match.group() == b'/' + b'home/*' and prefix.endswith(b'chrome:/'):
            continue
        # Emscripten declares a synthetic home inside its in-memory filesystem.
        # Allow that exact literal, never paths to a real checkout beneath it.
        virtual_home = b'/' + b'home/web_user'
        if match.group() == virtual_home and data[match.end():match.end() + 1] in (b'\"', b"'", b'\x00', b''):
            continue
        return True
    return bool(PRIVATE_HOST.search(data))


def assert_private_paths_absent(directory):
    for path in sorted(Path(directory).rglob('*')):
        if path.is_file() and path.stat().st_size:
            # Electron frameworks are large; scan mapped bytes without copying
            # the entire binary into the Python heap.
            with path.open('rb') as source, mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_READ) as data:
                if private_content(data):
                    raise ValueError(f'Private machine path or tailnet hostname in {path.relative_to(directory)}')


def public_tar_member(member):
    member.uid = member.gid = 0
    member.uname = member.gname = ''
    member.mtime = 0
    # Source names are provided explicitly, so no filesystem xattrs are needed.
    member.pax_headers = {}
    return member

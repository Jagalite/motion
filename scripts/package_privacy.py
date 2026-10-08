"""Privacy checks for distributable files and archive metadata."""
import re
from pathlib import Path

# Absolute developer-machine paths are never needed to run a release.
PRIVATE_PATH = re.compile(rb'/(?:Users|home|Volumes)/[^/\s\x00"\x27<>]+')
PRIVATE_HOST = re.compile(rb'[a-zA-Z0-9.-]+\.ts\.net')


def private_content(data):
    for match in PRIVATE_PATH.finditer(data):
        # Emscripten declares a synthetic home inside its in-memory filesystem.
        # Allow that exact literal, never paths to a real checkout beneath it.
        virtual_home = b'/' + b'home/web_user'
        if match.group() == virtual_home and data[match.end():match.end() + 1] in (b'\"', b"'", b'\x00', b''):
            continue
        return True
    return bool(PRIVATE_HOST.search(data))


def assert_private_paths_absent(directory):
    for path in sorted(Path(directory).rglob('*')):
        if path.is_file() and private_content(path.read_bytes()):
            raise ValueError(f'Private machine path or tailnet hostname in {path.relative_to(directory)}')


def public_tar_member(member):
    member.uid = member.gid = 0
    member.uname = member.gname = ''
    member.mtime = 0
    # Source names are provided explicitly, so no filesystem xattrs are needed.
    member.pax_headers = {}
    return member

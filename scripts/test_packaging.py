import base64
import hashlib
import io
import tarfile
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from build_media_tools import assert_portable, download
from package_app import npm_archive
from package_privacy import assert_private_paths_absent, public_tar_member


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.cache = Path(self.temp.name)

    def test_archive_owner_and_extended_metadata_are_removed(self):
        member = tarfile.TarInfo('Motion/readme.txt')
        member.uid, member.gid = 501, 20
        member.uname, member.gname = 'private-user', 'staff'
        member.mtime = 123.456
        member.pax_headers = {'SCHILY.xattr.owner': 'private-user', 'atime': '456'}
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode='w') as archive:
            archive.addfile(public_tar_member(member))
        stream.seek(0)
        with tarfile.open(fileobj=stream) as archive:
            result = archive.getmembers()[0]
            self.assertEqual((result.uid, result.gid, result.uname, result.gname), (0, 0, '', ''))
            self.assertEqual(result.pax_headers, {})
            self.assertEqual(result.mtime, 0)

    def test_binary_embedded_paths_and_private_hosts_block_release(self):
        binary = self.cache / 'motion'
        for contents in [b'prefix\x00/' + b'Users/private-account/source.rs\x00',
                         b'https://device.private-tailnet' + b'.ts.net']:
            binary.write_bytes(contents)
            with self.assertRaisesRegex(ValueError, 'Private machine'):
                assert_private_paths_absent(self.cache)
        binary.write_bytes(b'/build/cargo/source.rs\x00Copyright upstream author')
        assert_private_paths_absent(self.cache)

    def test_virtual_wasm_home_is_not_a_developer_checkout(self):
        binary = self.cache / 'engine.mjs'
        binary.write_bytes(b'HOME:"/' + b'home/web_user"')
        assert_private_paths_absent(self.cache)
        binary.write_bytes(b'/' + b'home/web_user/private/source.rs')
        with self.assertRaisesRegex(ValueError, 'Private machine'):
            assert_private_paths_absent(self.cache)

    def test_cached_source_must_match_pin(self):
        spec = {'archive': 'source.tar.gz', 'url': 'https://example.invalid/source',
                'sha256': hashlib.sha256(b'correct').hexdigest()}
        (self.cache / spec['archive']).write_bytes(b'corrupt')
        with patch('build_media_tools.subprocess.run') as run:
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                download(spec, self.cache)
            run.assert_not_called()

    def test_corrupt_download_is_not_published_to_cache(self):
        spec = {'archive': 'source.tar.gz', 'url': 'https://example.invalid/source', 'sha256': '0' * 64}
        def corrupt(command, **kwargs):
            Path(command[command.index('--output') + 1]).write_bytes(b'bad download')
        with patch('build_media_tools.subprocess.run', side_effect=corrupt):
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                download(spec, self.cache)
        self.assertFalse((self.cache / spec['archive']).exists())

    def test_npm_checks_both_registry_integrity_and_archive_pin(self):
        archive = self.cache / 'demuxe-1.0.0.tgz'
        archive.write_bytes(b'package')
        spec = {'version': '1.0.0', 'sha256': hashlib.sha256(b'package').hexdigest(),
                'integrity': 'sha512-' + base64.b64encode(hashlib.sha512(b'package').digest()).decode()}
        self.assertEqual(npm_archive({'demuxe': spec}, self.cache), archive)
        spec['integrity'] = 'sha512-wrong'
        with self.assertRaisesRegex(ValueError, 'integrity mismatch'):
            npm_archive({'demuxe': spec}, self.cache)

    def test_homebrew_linkage_is_rejected(self):
        with patch('build_media_tools.subprocess.check_output', return_value='ffmpeg:\n\t/opt/homebrew/lib/libcodec.dylib (version 1)\n'):
            with self.assertRaisesRegex(ValueError, 'non-system library'):
                assert_portable(Path('ffmpeg'))
        with patch('build_media_tools.subprocess.check_output', return_value='ffmpeg:\n\t/usr/lib/libSystem.B.dylib (version 1)\n'):
            assert_portable(Path('ffmpeg'))


if __name__ == '__main__':
    unittest.main()

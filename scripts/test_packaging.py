import base64
import hashlib
import io
import json
import subprocess
import tarfile
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from check_source_companion import check as check_source_companion
from build_media_tools import assert_portable, download
from package_app import npm_archive, source_inventory, write_source_archive
from package_privacy import assert_private_paths_absent, public_tar_member


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.cache = Path(self.temp.name)

    def source_checkout(self):
        root = self.cache / 'checkout'
        root.mkdir()
        subprocess.run(['git', 'init', '-q', str(root)], check=True)
        (root / '.gitignore').write_text('target/\nconfig.local.json\n')
        for name in ('Cargo.toml', 'crates/motion-ui/Cargo.toml',
                     'contracts/api.json', 'packages/ui-bridge/player.js',
                     'ui-assets/motion.css', 'scripts/build.sh'):
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name)
        (root / 'scripts/build.sh').chmod(0o755)
        (root / 'target').mkdir()
        (root / 'target/binary').write_text('generated')
        (root / 'config.local.json').write_text('secret')
        return root

    def test_complete_source_round_trip_and_ignored_files(self):
        root = self.source_checkout()
        with patch('package_app.ROOT', root):
            inventory = source_inventory()
            self.assertIn('crates/motion-ui/Cargo.toml', inventory)
            self.assertIn('contracts/api.json', inventory)
            self.assertIn('packages/ui-bridge/player.js', inventory)
            self.assertNotIn('target/binary', inventory)
            self.assertNotIn('config.local.json', inventory)
            archive = self.cache / 'source.tar.gz'
            write_source_archive(archive, inventory)
        extracted = self.cache / 'extracted'
        with tarfile.open(archive) as bundle:
            bundle.extractall(extracted, filter='data')
        actual = {str(p.relative_to(extracted / 'motion')): hashlib.sha256(p.read_bytes()).hexdigest()
                  for p in (extracted / 'motion').rglob('*') if p.is_file()}
        self.assertEqual(actual, inventory)
        self.assertTrue((extracted / 'motion/scripts/build.sh').stat().st_mode & 0o111)

    def test_embedded_input_changes_and_membership_block_source_publication(self):
        root = self.source_checkout()
        with patch('package_app.ROOT', root):
            for name in ('contracts/api.json', 'ui-assets/motion.css',
                         'packages/ui-bridge/player.js', 'crates/motion-ui/Cargo.toml'):
                before = source_inventory()
                (root / name).write_text('changed')
                self.assertNotEqual(before, source_inventory())
                with self.assertRaisesRegex(ValueError, 'source changed'):
                    write_source_archive(self.cache / 'bad.tar.gz', before)
            before = source_inventory()
            (root / 'new-input.rs').write_text('new')
            with self.assertRaisesRegex(ValueError, 'source changed'):
                write_source_archive(self.cache / 'bad.tar.gz', before)
            subprocess.run(['git', 'add', '.'], cwd=root, check=True)
            (root / 'new-input.rs').unlink()
            with self.assertRaisesRegex(ValueError, 'missing'):
                source_inventory()

    def test_reconstruction_rejects_corruption_before_build(self):
        root = self.source_checkout()
        with patch('package_app.ROOT', root):
            inventory = source_inventory()
            archive = self.cache / 'source.tar.gz'
            write_source_archive(archive, inventory)
        inventory['contracts/api.json'] = '0' * 64
        manifest = self.cache / 'manifest.json'
        manifest.write_text(json.dumps({'source_files': inventory,
                                      'source_commit': 'test', 'source_dirty': True}))
        work = self.cache / 'rebuild'
        with patch('check_source_companion.subprocess.run') as run:
            with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                check_source_companion(archive, manifest, work)
            run.assert_not_called()
        self.assertEqual(json.loads((work / 'receipt.json').read_text())['status'], 'failed')

    def test_symlink_input_is_rejected(self):
        root = self.source_checkout()
        (root / 'linked').symlink_to(self.cache / 'outside')
        with patch('package_app.ROOT', root):
            with self.assertRaisesRegex(ValueError, 'not a regular file'):
                source_inventory()

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

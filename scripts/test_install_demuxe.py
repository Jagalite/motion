import io
import json
import pathlib
import tarfile
import tempfile
import unittest
from install_demuxe import install, digest


class InstallationTests(unittest.TestCase):
    def archive(self, root, name='demuxe', extra=None):
        path = root / (name.replace('/', '-') + '.tgz')
        files = {'package/package.json': json.dumps({'name': name, 'version': 'test'}),
                 'package/web/generated/internal/provider-build.js': 'export const providerDeploymentEnabled = true;'}
        files.update(extra or {})
        with tarfile.open(path, 'w:gz') as archive:
            for name, content in files.items():
                data = content.encode()
                info = tarfile.TarInfo(name)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        return path

    def test_browser_install_and_receipt(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            archive = self.archive(root)
            output = root / 'installed'
            receipt = install(archive, output)
            self.assertEqual(receipt['deployment'], 'browser-only')
            self.assertEqual(receipt['archive_sha256'], digest(archive))
            for name, expected in receipt['files'].items():
                self.assertEqual(digest(output / name), expected)
            self.assertEqual(len(json.loads((output / 'demuxe-providers.json').read_text())['providers']), 3)
            with self.assertRaisesRegex(ValueError, 'already exists'):
                install(archive, output)

    def test_failure_never_publishes_partial_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            output = root / 'installed'
            for archive in [self.archive(root, 'wrong'), self.archive(root, extra={'package/../escape': 'no'})]:
                with self.assertRaises(ValueError):
                    install(archive, output)
                self.assertFalse(output.exists())
            self.assertFalse((root / 'escape').exists())
            self.assertEqual(list(root.glob('.demuxe-install-*')), [])

    def test_provider_requires_official_composer(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            archive = self.archive(root)
            with self.assertRaisesRegex(ValueError, 'together'):
                install(archive, root / 'installed', [archive])
            self.assertFalse((root / 'installed').exists())


if __name__ == '__main__':
    unittest.main()

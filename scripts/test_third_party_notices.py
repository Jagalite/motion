"""Checks that dependency notices retain attribution and fail on incomplete inputs."""
import tempfile
from pathlib import Path
import unittest

from third_party_notices import notice_files, render


class NoticeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def package(self, name):
        root = self.root / name
        root.mkdir()
        (root / 'Cargo.toml').write_text('')
        return dict(id=name, name=name, version='1.0.0', license='MIT',
                    manifest_path=str(root / 'Cargo.toml'), source='registry+test')

    def test_preserves_nested_license_texts_and_separate_attributions(self):
        first, second = self.package('first'), self.package('second')
        for name in ('first', 'second'):
            root = self.root / name
            (root / 'LICENSES').mkdir()
            (root / 'LICENSES/MIT.txt').write_text('Shared license terms\n')
            (root / 'NOTICE').write_text(f'Copyright {name}\n')
        result = render(dict(packages=[second, first], workspace_members=[]), 'target', b'lock')
        self.assertEqual(result.count('Shared license terms'), 1)
        self.assertIn('Copyright first', result)
        self.assertIn('Copyright second', result)
        self.assertIn('LICENSES/MIT.txt -> text', result)
        self.assertEqual(result, render(dict(packages=[first, second], workspace_members=[]), 'target', b'lock'))
        self.assertNotEqual(result, render(dict(packages=[first, second], workspace_members=[]), 'target', b'changed lock'))

    def test_declared_custom_license_filename_is_included(self):
        package = self.package('custom')
        package['license_file'] = 'terms.txt'
        (self.root / 'custom/terms.txt').write_text('Custom terms\n')
        self.assertEqual(notice_files(package), [('terms.txt', 'Custom terms\n')])

    def test_missing_and_empty_license_files_fail(self):
        package = self.package('missing')
        with self.assertRaisesRegex(ValueError, 'No license/notice'):
            notice_files(package)
        (self.root / 'missing/LICENSE').write_text('')
        with self.assertRaisesRegex(ValueError, 'Empty license'):
            notice_files(package)

    def test_git_workspace_crate_uses_its_checkout_license(self):
        checkout = self.root / 'checkout'
        (checkout / 'crates').mkdir(parents=True)
        (checkout / '.cargo-ok').write_text('')
        (checkout / 'LICENSE').write_text('Repository terms\n')
        (checkout / 'README.md').write_text('Not a notice\n')
        crate = checkout / 'crates/member'
        crate.mkdir()
        (crate / 'Cargo.toml').write_text('')
        package = dict(id='member', name='member', version='1.0.0', license='MIT',
                       manifest_path=str(crate / 'Cargo.toml'), source='git+https://example.invalid/repo')
        self.assertEqual(notice_files(package), [('repository/LICENSE', 'Repository terms\n')])
        # Only git checkouts fall back; a registry package must ship its own notices.
        package['source'] = 'registry+test'
        with self.assertRaisesRegex(ValueError, 'No license/notice'):
            notice_files(package)
        # Without a checkout marker there is no repository root to trust.
        package['source'] = 'git+https://example.invalid/repo'
        (checkout / '.cargo-ok').unlink()
        with self.assertRaisesRegex(ValueError, 'No license/notice'):
            notice_files(package)

    def test_escaped_license_file_fails(self):
        package = self.package('escape')
        (self.root / 'outside.txt').write_text('Unrelated terms')
        package['license_file'] = '../outside.txt'
        with self.assertRaisesRegex(ValueError, 'escapes package'):
            notice_files(package)

    def test_missing_license_declaration_fails(self):
        package = self.package('undeclared')
        package['license'] = None
        with self.assertRaisesRegex(ValueError, 'Missing license declaration'):
            render(dict(packages=[package], workspace_members=[]), 'target', b'lock')


if __name__ == '__main__':
    unittest.main()

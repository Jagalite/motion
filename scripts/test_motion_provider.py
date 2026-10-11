import pathlib
import tempfile
import unittest

from build_motion_provider import implementation_identity


class ProviderIdentity(unittest.TestCase):
    def test_adapter_policy_changes_identity_even_with_identical_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            backend = pathlib.Path(directory) / 'shaka-backend.js'
            artifacts = {'runtime/shaka.js': 'unchanged'}
            backend.write_text('load(url, undefined)')
            before = implementation_identity(artifacts, backend)
            self.assertEqual(before, implementation_identity(artifacts, backend))
            backend.write_text('load(url, 0)')
            self.assertNotEqual(before, implementation_identity(artifacts, backend))
            self.assertEqual(artifacts, {'runtime/shaka.js': 'unchanged'})


if __name__ == '__main__':
    unittest.main()

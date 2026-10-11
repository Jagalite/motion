import io
import os
import subprocess
import tarfile
from unittest.mock import patch

import build_motion_provider as provider
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


class NpmAssembly(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('MOTION_NPM_TEST_CACHE'), 'Set MOTION_NPM_TEST_CACHE to an npm archive cache')
    def test_published_archive_assembles_with_fixture_shaka_assets(self):
        # Exercise actual npm bytes and real adapter/catalog tests, without
        # claiming that these fixture assets qualify the Shaka compiler/player.
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            archive = root / 'shaka-fixture.tar.gz'
            with tarfile.open(archive, 'w:gz') as bundle:
                for name, data in {'LICENSE': b'fixture',
                                   'dist/shaka-player.motion.js': b'// fixture player',
                                   'dist/shaka-player.transmuxer-worker.js': b'// fixture worker'}.items():
                    member = tarfile.TarInfo('shaka-player-5.2.11/' + name)
                    member.size = len(data)
                    bundle.addfile(member, io.BytesIO(data))
            run = subprocess.run
            def execute(command, **kwargs):
                if command == provider.BUILD or command[0] == 'patch' or any(str(v).endswith('test_paced_shaka.mjs') for v in command):
                    return subprocess.CompletedProcess(command, 0)
                return run(command, **kwargs)
            with patch.object(provider, 'SHAKA', {'sha256': provider.digest(archive)}), \
                 patch.object(provider, 'download', return_value=archive), \
                 patch.object(provider.subprocess, 'run', side_effect=execute), \
                 patch.object(provider.subprocess, 'check_output', return_value='fixture toolchain'):
                receipt = provider.build(root / 'output', pathlib.Path(os.environ['MOTION_NPM_TEST_CACHE']), root / 'source')
            self.assertEqual(receipt['deployment'], 'motion-paced-derivative')
            self.assertEqual(receipt['archive_sha256'], '40b66848483e338e10bf2928f83bf6d8e854413186c9517f6143f722c8b13f7f')
            self.assertFalse(receipt['upstream_release'])


if __name__ == '__main__':
    unittest.main()

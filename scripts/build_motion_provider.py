"""Rebuild Motion's explicitly identified paced-HLS derivative (run via Kyoto)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

from build_media_tools import download, digest
from install_demuxe import extract
from package_privacy import assert_private_paths_absent

ROOT = Path(__file__).resolve().parents[1]
CORE = ROOT / 'vendor/demuxe/demuxe-core-1.1.1-candidate.tgz'
CORE_HASH = '4c5ef3bc8ee8413406590545cbbdef6249bd6fb4ce96e6ce8338da1a22b51274'
SHAKA = {'url': 'https://codeload.github.com/shaka-project/shaka-player/tar.gz/refs/tags/v5.2.11',
         'archive': 'shaka-v5.2.11.tar.gz',
         'sha256': '360910cf83bfcd792867eec040af6ab2387822bd2c726b4e4ef2774635b16801'}
BUILD = ['python3', 'build/build.py', '--name', 'motion', '--mode', 'release',
         '--skip-ts', '+@complete', '-@ui', '-@cast', '-@ads']
ASSETS = {'shaka-player.motion.js': 'shaka-player.js',
          'shaka-player.transmuxer-worker.js': 'shaka-player.transmuxer-worker.js'}


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()


def implementation_identity(artifacts, backend):
    closure = {**artifacts, 'adapter/shaka-backend.js': digest(backend)}
    return 'sha256:' + hashlib.sha256(encoded(closure)).hexdigest()


def build(output, cache, source):
    if output.exists() or source.exists():
        raise ValueError('Choose fresh output and source-build directories')
    if digest(CORE) != CORE_HASH:
        raise ValueError('Vendored core archive changed')
    patch = ROOT / 'vendor/shaka/motion-paced.patch'
    inputs = {'core': CORE_HASH, 'shaka_source': SHAKA['sha256'], 'patch': digest(patch),
              'builder': digest(Path(__file__))}
    archive = download(SHAKA, cache)
    source.mkdir(parents=True)
    with tarfile.open(archive) as bundle:
        bundle.extractall(source, filter='data')
    upstream = source / 'shaka-player-5.2.11'
    subprocess.run(['patch', '--batch', '--forward', '-p1', '-i', str(patch)], cwd=upstream, check=True)
    subprocess.run(['node', '--test', str(ROOT / 'scripts/test_paced_shaka.mjs')],
                   env={**os.environ, 'MOTION_SHAKA_SOURCE': str(upstream)}, check=True)
    subprocess.run(BUILD, cwd=upstream, check=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.motion-provider-', dir=output.parent) as temp:
        stage = Path(temp) / 'deployment'
        metadata = extract(CORE, stage)
        if metadata['name'] != 'demuxe' or metadata['version'] != '1.1.1':
            raise ValueError('Unexpected core package identity')
        # The package is a source-visible, locally assembled core, not npm 1.1.1.
        backend = stage / 'web/generated/internal/shaka-backend.js'
        before = backend.read_text()
        anchor = 'player.configure({ streaming: { observeQualityChanges: true'
        if before.count(anchor) != 1:
            raise ValueError('Unexpected provider configuration site')
        backend.write_text(before.replace(anchor, "player.configure({ manifest: { hls: { motionPaced: source.format === 'hls' && source.streaming?.live === true } }, streaming: { observeQualityChanges: true"))
        # Motion generations begin at media time zero. Preparing at Shaka's
        # moving live edge can await an unproduced segment before Motion gets
        # a chance to seek to its requested position. Only the same HLS path
        # opted into motionPaced above starts at zero; DASH is unchanged.
        load = "await player.load(source.url, undefined, source.format === 'hls' ? 'application/x-mpegurl' : 'application/dash+xml');"
        configured = backend.read_text()
        if configured.count(load) != 1:
            raise ValueError('Unexpected provider startup site')
        backend.write_text(configured.replace(load, "await player.load(source.url, source.format === 'hls' && source.streaming?.live === true ? 0 : undefined, source.format === 'hls' ? 'application/x-mpegurl' : 'application/dash+xml');"))
        assets = []
        for name, deployed in ASSETS.items():
            target = stage / 'web/vendor' / deployed
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(upstream / 'dist' / name, target)
            path = str(target.relative_to(stage))
            assets.append({'id': path, 'path': path, 'sha256': digest(target),
                           'bytes': target.stat().st_size, 'dependencies': []})
        artifacts = {'runtime/' + a['path']: a['sha256'] for a in assets}
        # The adapter's initial-position policy is part of this implementation,
        # even when the compiled Shaka artifacts are byte-identical.
        identity = implementation_identity(artifacts, backend)
        subprocess.run(['node', '--experimental-vm-modules', '--test',
                        str(ROOT / 'scripts/test_motion_provider_adapter.mjs')],
                       env={**os.environ, 'MOTION_PROVIDER_BACKEND': str(backend)}, check=True)
        providers = [{'id': name, 'implementationIdentity': 'demuxe-browser-v1',
                      'technology': 'browser-native', 'delivery': ['browser', 'application-bundle'],
                      'applicationBuild': 'motion-demuxe-core-' + CORE_HASH,
                      'offers': [{'capability': capability, 'version': 1, 'profile': profile}]}
                     for name, capability, profile in [
                         ('browser-original', 'media.present.original', 'selected-source'),
                         ('browser-prepared', 'media.present.prepared', 'selected-streams'),
                         ('web-audio-gain', 'audio.gain', 'scalar')]]
        providers.append({'id': 'shaka-adaptive', 'implementationIdentity': identity,
                          'applicationBuild': identity, 'technology': 'mixed',
                          'delivery': ['browser', 'application-bundle', 'optional-assets'],
                          'assetIds': [a['id'] for a in assets],
                          'offers': [{'capability': 'media.play.adaptive', 'version': 1,
                                      'profile': 'authorized-manifest'}]})
        manifest = {'schema': 1, 'providerContractVersion': 1, 'providers': providers, 'assets': assets,
                    'revision': 'sha256:' + hashlib.sha256(encoded({'providers': providers, 'assets': assets})).hexdigest()}
        (stage / 'demuxe-providers.json').write_bytes(encoded(manifest))
        # Keep the runtime's exact-identity admission check. This derivative gets
        # its own inventory; product qualification is recorded separately.
        inventory = stage / 'web/generated/internal/provider-build.js'
        text, count = re.subn(r'("shaka-adaptive":")[^"]+', lambda m: m[1] + identity, inventory.read_text())
        if count != 1:
            raise ValueError('Missing exact provider identity slot')
        inventory.write_text(text)
        notices = stage / 'licenses/shaka'
        notices.mkdir(parents=True)
        shutil.copy2(upstream / 'LICENSE', notices / 'LICENSE')
        shutil.copy2(patch, notices / 'motion-paced.patch')
        (notices / 'MODIFICATIONS.txt').write_text('Motion derivative of Shaka 5.2.11: opt-in retained manifest bounds for paced conversions. The Demuxe Shaka adapter opts into paced HLS and starts generation preparation at media time zero. See motion-paced.patch and scripts/build_motion_provider.py in the source companion.\n')
        if inputs != {'core': digest(CORE), 'shaka_source': digest(archive),
                      'patch': digest(patch), 'builder': digest(Path(__file__))}:
            raise ValueError('Build inputs changed during compilation')
        receipt = {'name': 'demuxe', 'version': '1.1.1', 'archive_sha256': CORE_HASH,
                   'deployment': 'motion-paced-derivative', 'runtime_ownership': 'per-player',
                   'upstream_release': False, 'qualification': 'candidate',
                   'inputs': inputs, 'provider_identity': identity, 'build_command': BUILD,
                   'toolchain': {tool: subprocess.check_output(argv, stderr=subprocess.STDOUT, text=True).strip()
                                 for tool, argv in [('node', ['node', '--version']), ('java', ['java', '-version'])]},
                   'files': {str(p.relative_to(stage)): digest(p) for p in sorted(stage.rglob('*')) if p.is_file()}}
        (stage / 'playscale-package.json').write_bytes(encoded(receipt))
        assert_private_paths_absent(stage)
        stage.rename(output)
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cache', type=Path, required=True)
    parser.add_argument('--source-build', type=Path, required=True)
    args = parser.parse_args()
    result = build(args.output.resolve(), args.cache.resolve(), args.source_build.resolve())
    print(json.dumps({'provider_identity': result['provider_identity'], 'qualification': result['qualification']}))

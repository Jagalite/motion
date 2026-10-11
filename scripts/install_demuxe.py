"""Install an exact Demuxe core, optionally composed with provider archives.

Provider deployments use Demuxe's official deploy-providers.py supplied through
--composer; its hash and every input archive hash are recorded in the receipt.
No compilation, package discovery or network access is performed.
"""
import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def extract(path, output):
    with tarfile.open(path, 'r:gz') as archive:
        members = archive.getmembers()
        names = set()
        for member in members:
            name = pathlib.PurePosixPath(member.name)
            if (not name.parts or name.parts[0] != 'package' or '..' in name.parts
                    or name.is_absolute() or not (member.isfile() or member.isdir())
                    or member.name in names):
                raise ValueError('Unsafe package member')
            names.add(member.name)
        if sum(member.size for member in members) > 2 * 1024**3:
            raise ValueError('Package too large')
        for member in members:
            target = output.joinpath(*pathlib.PurePosixPath(member.name).parts[1:])
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.extractfile(member) as source, target.open('wb') as dest:
                    shutil.copyfileobj(source, dest)
    return json.loads((output / 'package.json').read_text())


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()


def install(core_archive, output, providers=(), composer=None):
    if output.exists():
        raise ValueError('Output already exists; use a new versioned destination')
    if bool(providers) != bool(composer):
        raise ValueError('--provider and --composer must be supplied together')
    output.parent.mkdir(parents=True, exist_ok=True)
    # Build the deployment beside its final destination, publishing only when
    # extraction, composition and the complete hash receipt have succeeded.
    with tempfile.TemporaryDirectory(prefix='.demuxe-install-', dir=output.parent) as scratch:
        scratch = pathlib.Path(scratch)
        core = scratch / 'core'
        package = extract(core_archive, core)
        if package.get('name') != 'demuxe':
            raise ValueError('Not a Demuxe package')
        receipt = {'name': package['name'], 'version': package['version'],
                   'archive_sha256': digest(core_archive)}
        deployment = scratch / 'deployment'
        if providers:
            command = [sys.executable, str(composer.resolve()), '--core', str(core.resolve())]
            inputs = []
            for index, archive in enumerate(providers):
                installed = scratch / f'provider-{index}'
                metadata = extract(archive, installed)
                command += ['--provider', str(installed.resolve())]
                inputs.append({'name': metadata['name'], 'version': metadata['version'], 'archive_sha256': digest(archive)})
            command += ['--output', str(deployment.resolve())]
            subprocess.run(command, check=True, capture_output=True, text=True)
            receipt.update(deployment='composed-providers', providers=inputs, composer_sha256=digest(composer))
        else:
            core.rename(deployment)
            flags = (deployment / 'web/generated/internal/provider-build.js').read_text()
            if 'providerDeploymentEnabled = true' in flags or 'providerDeploymentEnabled=true' in flags:
                facts = [{'id': identifier, 'implementationIdentity': 'demuxe-browser-v1',
                          'technology': 'browser-native', 'delivery': ['browser', 'application-bundle'],
                          'applicationBuild': 'demuxe-' + package['version'],
                          'offers': [{'capability': capability, 'version': 1, 'profile': profile}]}
                         for identifier, capability, profile in [
                             ('browser-original', 'media.present.original', 'selected-source'),
                             ('browser-prepared', 'media.present.prepared', 'selected-streams'),
                             ('web-audio-gain', 'audio.gain', 'scalar')]]
                manifest = {'schema': 1, 'providerContractVersion': 1,
                            'revision': 'sha256:' + hashlib.sha256(encoded({'providers': facts, 'assets': []})).hexdigest(),
                            'providers': facts, 'assets': []}
                (deployment / 'demuxe-providers.json').write_bytes(encoded(manifest))
                receipt['deployment'] = 'browser-only'
            else:
                receipt['deployment'] = 'upstream-bundled'
        receipt['files'] = {str(path.relative_to(deployment)): digest(path)
                            for path in sorted(deployment.rglob('*')) if path.is_file()}
        (deployment / 'playscale-package.json').write_text(json.dumps(receipt, indent=2) + '\n')
        deployment.rename(output)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=pathlib.Path)
    parser.add_argument('--output', type=pathlib.Path, default=pathlib.Path('web/vendor/demuxe'))
    parser.add_argument('--provider', type=pathlib.Path, action='append', default=[])
    parser.add_argument('--composer', type=pathlib.Path)
    args = parser.parse_args()
    try:
        receipt = install(args.archive, args.output, args.provider, args.composer)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'Demuxe installation failed: {error}\n')
    print(json.dumps({key: value for key, value in receipt.items() if key != 'files'}))


if __name__ == '__main__':
    main()

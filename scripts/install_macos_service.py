"""Install a per-user login service with an isolated executable and Demuxe assets."""
import argparse
import hashlib
import json
import os
import pathlib
import plistlib
import re
import time
import shutil
import subprocess
import sys


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=pathlib.Path)
    parser.add_argument('--config', required=True, type=pathlib.Path)
    parser.add_argument('--install-dir', type=pathlib.Path, default=pathlib.Path.home() / 'Library/Application Support/Playscale')
    parser.add_argument('--replace', action='store_true', help='Explicitly replace an existing Playscale login service/configuration')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('This installer requires macOS launchd')
    binary = args.binary.resolve(strict=True)
    config = json.loads(subprocess.check_output([str(binary), '--config', str(args.config.resolve(strict=True)), '--check-config'], text=True))
    # CLI/file-relative paths become absolute before leaving the working checkout.
    for key in ['data_dir', 'demuxe_dir']:
        config[key] = str(pathlib.Path(config[key]).resolve())
    config['libraries'] = [str(pathlib.Path(p).resolve()) for p in config['libraries']]
    probe = shutil.which(config['ffprobe'])
    if probe is None:
        parser.error('FFprobe must be installed before configuring the service')
    config['ffprobe'] = probe
    encoder = shutil.which(config['processing']['ffmpeg'])
    if encoder is None:
        parser.error('FFmpeg must be installed before configuring the service')
    config['processing']['ffmpeg'] = encoder
    assets = pathlib.Path(config['demuxe_dir'])
    if not (assets / 'web/generated/player/index.js').is_file():
        parser.error('Matching Demuxe installation is required')
    def tree_digest(directory):
        tree = hashlib.sha256()
        for path in sorted(directory.rglob('*')):
            if path.is_symlink():
                parser.error('Demuxe assets must not contain symlinks')
            if path.is_file():
                tree.update(str(path.relative_to(directory)).encode() + b'\0' + digest(path).encode())
        return tree.hexdigest()
    asset_digest = tree_digest(assets)
    release_id = hashlib.sha256((digest(binary) + asset_digest).encode()).hexdigest()[:24]
    root = args.install_dir.expanduser().resolve()
    label = 'local.playscale.server'
    plist_path = pathlib.Path.home() / 'Library/LaunchAgents' / (label + '.plist')
    installed_config = root / 'config.json'
    if (plist_path.exists() or installed_config.exists()) and not args.replace:
        parser.error('Existing service/configuration found; use --replace for an intentional upgrade')
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    tools = root / 'tools'
    tools.mkdir(exist_ok=True, mode=0o700)
    shutil.copy2(pathlib.Path(__file__).resolve().with_name('backup.py'), tools / 'backup.py')
    logs = root / 'logs'
    logs.mkdir(exist_ok=True, mode=0o700)
    release = root / 'releases' / release_id
    if not release.exists():
        release.mkdir(parents=True, mode=0o700)
        shutil.copy2(binary, release / 'playscale')
        shutil.copytree(assets, release / 'demuxe')
        (release / 'release.json').write_text(json.dumps({'binary_sha256': digest(binary), 'demuxe_tree_sha256': asset_digest}, indent=2) + '\n')
    if digest(release / 'playscale') != digest(binary) or tree_digest(release / 'demuxe') != asset_digest:
        parser.error('Existing release is incomplete or does not match; refusing activation')
    config['demuxe_dir'] = str(release / 'demuxe')
    temporary = root / 'config.pending.json'
    temporary.write_text(json.dumps(config, indent=2) + '\n')
    os.chmod(temporary, 0o600)
    subprocess.run([str(release / 'playscale'), '--config', str(temporary), '--check-config'], check=True, stdout=subprocess.DEVNULL)
    domain = f'gui/{os.getuid()}'
    service = domain + '/' + label
    if args.replace:
        running = subprocess.run(['launchctl', 'print', service], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        if running.returncode == 0:
            match = re.search(r'^\s*pid = (\d+)$', running.stdout, re.MULTILINE)
            subprocess.run(['launchctl', 'bootout', service], check=True)
            if match:
                deadline = time.monotonic() + 25
                while True:
                    try:
                        os.kill(int(match.group(1)), 0)
                    except ProcessLookupError:
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError('Previous service did not exit; new configuration has not been activated')
                    time.sleep(.1)
    if installed_config.exists():
        shutil.copy2(installed_config, root / "config.previous.json")
    if plist_path.exists():
        shutil.copy2(plist_path, root / "service.previous.plist")
    temporary.replace(installed_config)
    plist = {'Label': label, 'ProgramArguments': [str(release / 'playscale'), '--config', str(installed_config)], 'WorkingDirectory': str(root), 'RunAtLoad': True, 'KeepAlive': True, 'ThrottleInterval': 10, 'ExitTimeOut': 20, 'ProcessType': 'Background', 'StandardOutPath': str(logs / 'stdout.log'), 'StandardErrorPath': str(logs / 'stderr.log'), 'EnvironmentVariables': {'PATH': '/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin', 'RUST_LOG': 'playscale=info,tower_http=info'}}
    plist_path.parent.mkdir(parents=True, exist_ok=True)
    pending = plist_path.with_suffix('.pending')
    with pending.open('wb') as stream:
        plistlib.dump(plist, stream)
    os.chmod(pending, 0o600)
    pending.replace(plist_path)
    subprocess.run(['launchctl', 'enable', service], check=True)
    subprocess.run(['launchctl', 'bootstrap', domain, str(plist_path)], check=True)
    print(json.dumps({'service': service, 'config': str(installed_config), 'release': str(release), 'logs': str(logs), 'public_origin': config['public_origin'], 'binary_sha256': digest(binary), 'demuxe_tree_sha256': asset_digest}, indent=2))


if __name__ == '__main__':
    main()

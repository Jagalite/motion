"""Assemble the native application shell around already-built owned components."""
import json
from pathlib import Path
import plistlib
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def assemble_shell(directory, electron, server, helper, terminal, media, deployment):
    """All runtime files live in Motion.app; outer symlinks are CLI conveniences."""
    expected = json.loads((ROOT / 'apps/desktop/package.json').read_text())['devDependencies']['electron']
    if (electron / 'version').read_text().strip() != expected:
        raise ValueError('Electron distribution does not match the lockfile version')
    app = directory / 'Motion.app'
    shutil.copytree(electron / 'Electron.app', app, symlinks=True)
    contents = app / 'Contents'
    resources = contents / 'Resources'
    default = resources / 'default_app.asar'
    if default.exists():
        default.unlink()  # Only the newly copied distribution, never an installed app.
    executable = contents / 'MacOS/Electron'
    executable.rename(contents / 'MacOS/Motion')
    plist = contents / 'Info.plist'
    info = plistlib.loads(plist.read_bytes())
    info.update(CFBundleExecutable='Motion', CFBundleName='Motion', CFBundleDisplayName='Motion',
                CFBundleIdentifier='org.motion.desktop', CFBundleShortVersionString='0.1.0', CFBundleVersion='1')
    plist.write_bytes(plistlib.dumps(info))
    for name, source in [('motion-server', server), ('motion-ui-host', helper), ('motion', terminal)]:
        shutil.copy2(source, resources / name)
    shutil.copytree(media, resources / 'tools')
    shutil.copytree(deployment, resources / 'assets/demuxe')
    (resources / 'contracts').mkdir()
    shutil.copy2(ROOT / 'contracts/Motion_Server_API_v2.yaml', resources / 'contracts/Motion_Server_API_v2.yaml')
    client = resources / 'app'
    shutil.copytree(ROOT / 'apps/desktop/src', client / 'src')
    metadata = json.loads((ROOT / 'apps/desktop/package.json').read_text())
    metadata.pop('devDependencies', None)
    metadata.pop('scripts', None)
    metadata.update(name='motion', productName='Motion')
    (client / 'package.json').write_text(json.dumps(metadata, indent=2) + '\n')
    # Package adapter marker; the complete signed-file inventory lives outside
    # the app, avoiding a self-referential hash and post-signing mutation.
    (resources / 'motion-package.json').write_text('{"schema":1}\n')
    licenses = resources / 'licenses/electron'
    licenses.mkdir(parents=True)
    for name in ('LICENSE', 'LICENSES.chromium.html'):
        shutil.copy2(electron / name, licenses / name)
    for name in ('motion', 'motion-server'):
        (directory / name).symlink_to('Motion.app/Contents/Resources/' + name)
    launcher = directory / 'Motion.command'
    launcher.write_text('#!/bin/sh\nexec "$(/usr/bin/dirname -- "$0")/Motion.app/Contents/MacOS/Motion" "$@"\n')
    launcher.chmod(0o755)
    return resources


def sign_shell(directory):
    # Run only after all application files and notices have been installed.
    app = directory / 'Motion.app'
    subprocess.run(['/usr/bin/codesign', '--force', '--deep', '--sign', '-', str(app)], check=True)
    subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app)], check=True)


def go_notices(destination):
    text = subprocess.check_output(['go', 'list', '-deps', '-json', './cmd/motion'],
                                   cwd=ROOT / 'apps/terminal', text=True)
    decoder = json.JSONDecoder()
    modules = {}
    while text.strip():
        item, end = decoder.raw_decode(text.lstrip())
        text = text.lstrip()[end:]
        module = item.get('Module')
        if module and not module.get('Main'):
            modules[module['Path']] = module
    inventory = []
    for name, module in sorted(modules.items()):
        source = Path(module['Dir'])
        notices = [p for p in source.iterdir() if p.is_file()
                   and p.name.upper().startswith(('LICENSE', 'COPYING', 'NOTICE'))]
        if not notices:
            raise ValueError('Missing Go module license: ' + name)
        target = destination / (name + '@' + module['Version'])
        target.mkdir(parents=True)
        for notice in notices:
            shutil.copy2(notice, target / notice.name)
        inventory.append({'module': name, 'version': module['Version'],
                          'notices': [p.name for p in notices]})
    goroot = Path(subprocess.check_output(['go', 'env', 'GOROOT'], text=True).strip())
    shutil.copy2(goroot / 'LICENSE', destination / 'Go-LICENSE')
    (destination / 'modules.json').write_text(json.dumps(inventory, indent=2) + '\n')

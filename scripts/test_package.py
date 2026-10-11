"""Exercise an extracted Motion archive with no development tools on PATH."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile

from package_privacy import assert_private_paths_absent
from smoke import free_port, request, wait_for, stop
from v2_smoke_support import Client


def sha(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def exercise(archive, work, keep_running=False):
    work.mkdir(parents=True, exist_ok=False)
    with tarfile.open(archive) as bundle:
        for member in bundle.getmembers():
            assert (member.uid, member.gid, member.uname, member.gname) == (0, 0, '', '')
            assert set(member.pax_headers) <= {'path', 'linkpath'}, member.name
        bundle.extractall(work, filter='data')
    app = work / 'Relocated Motion With Spaces'
    (work / 'Motion').rename(app)
    assert_private_paths_absent(app)
    manifest = json.loads((app / 'motion-package.json').read_text())
    for name, expected in manifest['files'].items():
        assert sha(app / name) == expected, name
    clean_env = {key: value for key, value in os.environ.items()
                 if not key.startswith(('DYLD_', 'LD_', 'CARGO_', 'RUST', 'DEMUXE'))}
    clean_env['PATH'] = '/nonexistent-motion-test-path'
    resources = app / 'Motion.app/Contents/Resources'
    binary = app / 'motion-server'
    cli = app / 'motion'
    subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app / 'Motion.app')], check=True)
    help_text = subprocess.check_output([str(cli), '--help'], cwd=work, env=clean_env, text=True)
    assert 'tui' in help_text and 'serve' in help_text
    configured = json.loads(subprocess.check_output([str(binary), '--check-config'], cwd=work, env=clean_env))
    assert Path(configured['ffprobe']) == resources / 'tools/ffprobe'
    assert Path(configured['processing']['ffmpeg']) == resources / 'tools/ffmpeg'
    assert Path(configured['demuxe_dir']) == resources / 'assets/demuxe'
    launched = json.loads(subprocess.check_output([str(cli), 'serve', '--check-config'], cwd=work, env=clean_env))
    assert launched == configured
    media = work / 'media'
    media.mkdir()
    fixture = media / 'Motion-package-fixture.mp4'
    subprocess.run([str(resources / 'tools/ffmpeg'), '-v', 'error', '-f', 'lavfi', '-i',
                    'testsrc2=size=320x180:rate=24', '-f', 'lavfi', '-i',
                    'sine=frequency=440:sample_rate=48000', '-t', '8', '-c:v', 'libx264',
                    '-threads', '2', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-movflags', '+faststart',
                    str(fixture)], cwd=work, env=clean_env, check=True)
    probe = json.loads(subprocess.check_output([str(resources / 'tools/ffprobe'), '-v', 'error',
                        '-show_streams', '-show_format', '-of', 'json', str(fixture)], env=clean_env))
    assert {stream['codec_name'] for stream in probe['streams']} == {'h264', 'aac'}
    state = work / 'state'
    port = free_port()
    log = (work / 'server.log').open('wb')
    command = [str(binary), '--listen', f'127.0.0.1:{port}', '--data-dir', str(state), '--library', str(media)]
    process = subprocess.Popen(command, cwd=work, env=clean_env, stdout=log, stderr=log)
    checks = ['app_signature', 'bundled_go_cli', 'archive_privacy', 'binary_privacy', 'archive_inventory', 'relocated_bundle_defaults', 'bundled_encoding_and_probe']
    success = False
    try:
        wait_for(lambda: request(port, 'GET', '/ready')[0] == 200, seconds=45)
        client = Client(port, (state / 'admin-token').read_text().strip())
        api = client.json
        assert request(port, 'GET', '/api/v2/catalog/items')[0] == 401
        checks.append('restricted_default_refuses_anonymous_catalog')
        def catalog():
            return next(iter(api('GET', '/api/v2/catalog/items')['items']), None)
        item = wait_for(catalog, seconds=45)
        file = api('GET', f'/api/v2/catalog/items/{item["id"]}/files')['items'][0]
        timeline = api('GET', f'/api/v2/catalog/items/{item["id"]}/timelines')['items'][0]['id']
        assert abs(file['duration_ms'] - 8000) < 100
        checks.append('server_scan_with_bundled_ffprobe')
        delivery = client.original_delivery(timeline)
        media_url = delivery['active']['media_url']
        payload, _ = client.call('GET', media_url)
        assert hashlib.sha256(payload).hexdigest() == sha(fixture)
        payload, _ = client.call('GET', media_url, expected=206, headers={'Range': 'bytes=0-63'})
        assert payload == fixture.read_bytes()[:64]
        api('DELETE', '/api/v2/playback/delivery-sessions/' + delivery['id'], expected=204)
        checks.append('media_bytes_and_ranges')
        status, _, payload = request(port, 'GET', '/assets/demuxe/package.json')
        assert status == 200 and json.loads(payload)['version'] == manifest['demuxe']['version']
        checks.append('npm_demuxe_served')
        job = api('POST', '/api/v2/processing/preparations', {
            'source': {'file_id': file['id'], 'file_revision': file['revision']},
            'timeline_id': timeline, 'recipe_id': 'h264720p',
            'tracks': {'audio_component_id': None, 'subtitle_component_id': None,
                       'subtitle_policy': 'off', 'audio_track_id': None, 'subtitle_track_id': None}}, 202)
        def completed():
            row = api('GET', '/api/v2/jobs/' + job['id'])
            if row['phase'] in ('failed', 'cancelled'):
                raise AssertionError(row)
            return row if row['phase'] == 'completed' else None
        result = wait_for(completed, seconds=120)
        assert result['result_ids']
        checks.append('server_transcode_with_bundled_ffmpeg')
        delivery = client.original_delivery(timeline)
        epoch = client.viewing_epoch()
        viewing = api('POST', '/api/v2/playback/viewing-sessions',
                      {'delivery_id': delivery['id'], 'expected_viewing_revision': '0'}, 201, epoch)
        api('POST', f'/api/v2/playback/viewing-sessions/{viewing["id"]}/events',
            {'event_id': 'package-progress', 'sequence': '1', 'delivery_generation': delivery['active']['generation'],
             'position_ms': 2500, 'status': 'paused'}, headers=epoch)
        stop(process)
        process = subprocess.Popen(command, cwd=work, env=clean_env, stdout=log, stderr=log)
        wait_for(lambda: request(port, 'GET', '/api/v2/system/health')[0] == 200, seconds=45)
        assert api('GET', f'/api/v2/profiles/default/timelines/{timeline}/viewing')['position_ms'] == 2500
        checks.append('restart_retains_progress')
        receipt = {'checks': checks, 'archive_sha256': sha(archive), 'port': port,
                   'pid': process.pid, 'app': app.name, 'item_id': item['id'],
                   'media_url': media_url, 'processing_job': result,
                   'path': clean_env['PATH'], 'browser': 'not yet run'}
        (work / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
        print(json.dumps(receipt, indent=2))
        success = True
    finally:
        if not (success and keep_running):
            stop(process)
        log.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--keep-running', action='store_true', help='Leave successful server for browser tests')
    args = parser.parse_args()
    exercise(args.archive.resolve(), args.work.resolve(), args.keep_running)

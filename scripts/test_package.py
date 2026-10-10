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
    binary = app / 'motion'
    configured = json.loads(subprocess.check_output([str(binary), '--check-config'], cwd=work, env=clean_env))
    assert Path(configured['ffprobe']) == app / 'tools/ffprobe'
    assert Path(configured['processing']['ffmpeg']) == app / 'tools/ffmpeg'
    assert Path(configured['demuxe_dir']) == app / 'assets/demuxe'
    launched = json.loads(subprocess.check_output([str(app / 'Motion.command'), '--check-config'], cwd=work, env=clean_env))
    assert launched == configured
    media = work / 'media'
    media.mkdir()
    fixture = media / 'Motion-package-fixture.mp4'
    subprocess.run([str(app / 'tools/ffmpeg'), '-v', 'error', '-f', 'lavfi', '-i',
                    'testsrc2=size=320x180:rate=24', '-f', 'lavfi', '-i',
                    'sine=frequency=440:sample_rate=48000', '-t', '8', '-c:v', 'libx264',
                    '-threads', '2', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-movflags', '+faststart',
                    str(fixture)], cwd=work, env=clean_env, check=True)
    probe = json.loads(subprocess.check_output([str(app / 'tools/ffprobe'), '-v', 'error',
                        '-show_streams', '-show_format', '-of', 'json', str(fixture)], env=clean_env))
    assert {stream['codec_name'] for stream in probe['streams']} == {'h264', 'aac'}
    state = work / 'state'
    port = free_port()
    log = (work / 'server.log').open('wb')
    command = [str(binary), '--listen', f'127.0.0.1:{port}', '--data-dir', str(state), '--library', str(media)]
    process = subprocess.Popen(command, cwd=work, env=clean_env, stdout=log, stderr=log)
    checks = ['archive_privacy', 'binary_privacy', 'archive_inventory', 'relocated_bundle_defaults', 'bundled_encoding_and_probe']
    success = False
    try:
        wait_for(lambda: request(port, 'GET', '/ready')[0] == 200, seconds=45)
        # The package runs in its default restricted access mode: the legacy
        # API is closed to anonymous clients and open to the operator token.
        auth = {'Authorization': 'Bearer ' + (state / 'admin-token').read_text().strip()}
        assert request(port, 'GET', '/api/v1/items')[0] in (401, 403)
        checks.append('restricted_default_refuses_anonymous_legacy')
        def catalog():
            status, _, raw = request(port, 'GET', '/api/v1/items', headers=auth)
            assert status == 200, (status, raw)
            result = json.loads(raw)['items']
            return result[0] if result else None
        item = wait_for(catalog, seconds=45)
        assert abs(item['duration_seconds'] - 8) < .1
        checks.append('server_scan_with_bundled_ffprobe')
        status, headers, payload = request(port, 'GET', item['media_url'], headers=auth)
        assert status == 200 and hashlib.sha256(payload).hexdigest() == sha(fixture)
        status, _, payload = request(port, 'GET', item['media_url'], headers={**auth, 'Range': 'bytes=0-63'})
        assert status == 206 and payload == fixture.read_bytes()[:64]
        checks.append('media_bytes_and_ranges')
        status, _, payload = request(port, 'GET', '/assets/demuxe/package.json')
        assert status == 200 and json.loads(payload)['version'] == manifest['demuxe']['version']
        checks.append('npm_demuxe_served')
        status, _, payload = request(port, 'POST', '/api/v1/processing-jobs', {
            'source_file_id': item['file_id'], 'source_revision': item['revision'],
            'recipe': 'h264720p', 'backend': 'software', 'idempotency_key': 'package-test'}, auth)
        assert status == 201, (status, payload)
        job = json.loads(payload)
        def completed():
            row = json.loads(request(port, 'GET', '/api/v1/processing-jobs/' + job['id'], headers=auth)[2])
            if row['phase'] in ('failed', 'cancelled'):
                raise AssertionError(row)
            return row if row['phase'] == 'completed' else None
        result = wait_for(completed, seconds=120)
        assert result['output_file_id']
        checks.append('server_transcode_with_bundled_ffmpeg')
        progress = f'/api/v1/profiles/default/progress/{item["id"]}'
        assert request(port, 'PUT', progress, {'position_seconds': 2.5}, auth)[0] == 200
        stop(process)
        process = subprocess.Popen(command, cwd=work, env=clean_env, stdout=log, stderr=log)
        wait_for(lambda: request(port, 'GET', '/ready')[0] == 200, seconds=45)
        auth = {'Authorization': 'Bearer ' + (state / 'admin-token').read_text().strip()}
        assert json.loads(request(port, 'GET', progress, headers=auth)[2])['position_seconds'] == 2.5
        checks.append('restart_retains_progress')
        receipt = {'checks': checks, 'archive_sha256': sha(archive), 'port': port,
                   'pid': process.pid, 'app': app.name, 'item_id': item['id'],
                   'media_url': item['media_url'], 'processing_job': result,
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

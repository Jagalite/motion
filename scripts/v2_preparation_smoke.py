"""Real v2 preparation and backup acceptance; uses an explicitly built binary.
No builds, user libraries, or shared settings are touched.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess
import tempfile
from smoke import free_port, request, wait_for, stop


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=pathlib.Path, required=True)
    parser.add_argument('--receipt', type=pathlib.Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    checks = []
    with tempfile.TemporaryDirectory(prefix='motion-v2-preparation-') as directory:
        root = pathlib.Path(directory)
        media = root / 'media'
        media.mkdir()
        state = root / 'state'
        subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=160x90:rate=24',
                        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=44100',
                        '-f', 'lavfi', '-i', 'sine=frequency=880:sample_rate=22050',
                        '-t', '3', '-map', '0', '-map', '1', '-map', '2', '-c:v', 'libx264',
                        '-threads', '1', '-preset', 'ultrafast', '-c:a', 'aac', str(media / 'Film.mp4')], check=True)
        port = free_port()
        log = (root / 'server.log').open('wb')
        process = subprocess.Popen([str(binary), '--access-mode', 'restricted', '--listen', f'127.0.0.1:{port}',
                                    '--data-dir', str(state), '--library', str(media)], stdout=log, stderr=subprocess.STDOUT)
        try:
            wait_for(lambda: request(port, 'GET', '/ready')[0] == 200, seconds=120)
            auth = {'Authorization': 'Bearer ' + (state / 'admin-token').read_text().strip()}

            def api(method, path, body=None, expected=200, key=None):
                headers = dict(auth)
                if key:
                    headers['Idempotency-Key'] = key
                status, h, data = request(port, method, '/api/v2' + path, body, headers)
                value = json.loads(data) if data else None
                assert status == expected, (method, path, status, value)
                return h, value

            item = wait_for(lambda: next(iter(api('GET', '/catalog/items')[1]['items']), None), seconds=60)
            timeline = api('GET', f"/catalog/items/{item['id']}/timelines")[1]['items'][0]['id']
            version = api('GET', f'/catalog/timelines/{timeline}/versions')[1]['items'][0]
            source = version['files'][0]['file']
            _, recipes = api('GET', '/processing/recipes')
            assert 'remux_mp4' in [r['id'] for r in recipes['items']]
            body = {'source': source, 'timeline_id': timeline, 'recipe_id': 'remux_mp4',
                    'tracks': {'audio_component_id': None, 'subtitle_component_id': None,
                               'subtitle_policy': 'off', 'audio_track_id': 'a1', 'subtitle_track_id': None}}
            _, job = api('POST', '/processing/preparations', body, 202, 'preparation-acceptance')
            replay_headers, replay = api('POST', '/processing/preparations', body, 202, 'preparation-acceptance')
            assert replay == job and replay_headers.get('idempotent-replayed') == 'true'
            checks.append('v2_preparation_exact_replay')

            def finished():
                row = api('GET', f"/jobs/{job['id']}")[1]
                return row if row['phase'] not in ('queued', 'running', 'cancelling') else None
            completed = wait_for(finished, seconds=120)
            assert completed['phase'] == 'completed', completed
            rendition = next(r for r in api('GET', '/processing/renditions')[1]['items'] if r['job_id'] == job['id'])
            assert rendition['status'] == 'ready' and rendition['timeline_id'] == timeline
            assert rendition['output']['file_id'] in completed['result_ids']
            versions = api('GET', f'/catalog/timelines/{timeline}/versions')[1]['items']
            assert any(v['origin'] == 'generated' and v['files'][0]['file'] == rendition['output'] for v in versions)
            output = rendition['output']
            status, _, payload = request(port, 'GET', f"/api/v2/media/files/{output['file_id']}/content?revision={output['file_revision']}", headers=auth)
            assert status == 200, (status, payload[:1000])
            assert hashlib.sha256(payload).hexdigest() == output['file_revision']
            prepared = root / 'prepared.mp4'
            prepared.write_bytes(payload)
            tracks = json.loads(subprocess.run(['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(prepared)], check=True, capture_output=True).stdout)['streams']
            assert [t['sample_rate'] for t in tracks if t['codec_type'] == 'audio'] == ['22050']
            checks.extend(['requested_timeline_publication', 'selected_second_audio_track', 'revision_pinned_output_bytes'])
            _, backup = api('POST', '/admin/backups', {'include_generated_media': True}, 202, 'backup-acceptance')

            def backed_up():
                row = next(b for b in api('GET', '/admin/backups')[1]['items'] if b['id'] == backup['id'])
                return row if row['status'] != 'pending' else None
            backup = wait_for(backed_up, seconds=120)
            assert backup['status'] == 'ready', backup
            manifest_path = state / 'backups' / f"request-{backup['id']}" / 'manifest.json'
            manifest_bytes = manifest_path.read_bytes()
            manifest = json.loads(manifest_bytes)
            assert backup['manifest_digest'] == 'sha256:' + hashlib.sha256(manifest_bytes).hexdigest()
            entry = next(m for m in manifest['generated_media'] if m['file_id'] == output['file_id'])
            assert (manifest_path.parent / entry['path']).read_bytes() == payload
            checks.append('backup_generated_media_manifest_and_bytes')
        except BaseException:
            log.flush()
            print((root / 'server.log').read_text(errors='replace')[-16000:])
            raise
        finally:
            stop(process)
            log.close()
    receipt = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'checks': checks,
               'passed': len(checks), 'scope': 'Real HTTP v2 preparation, FFmpeg, FFprobe and generated-media backup; no browser claim'}
    args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()

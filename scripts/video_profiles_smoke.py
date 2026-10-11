"""Real v2 HTTP + FFmpeg profile qualification; no browser or hardware claim."""
import argparse
import hashlib
import http.client
import json
import pathlib
import shutil
import subprocess
import tempfile
import time
from smoke import free_port, request, wait_for, stop
from v2_smoke_support import Client


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=pathlib.Path, required=True)
    parser.add_argument('--receipt', type=pathlib.Path)
    args = parser.parse_args()
    results = []
    with tempfile.TemporaryDirectory(prefix='motion-profiles-') as directory:
        root = pathlib.Path(directory)
        media = root / 'media'
        media.mkdir()
        state = root / 'state'
        port = free_port()
        source_path = media / 'source.mp4'
        ffmpeg = shutil.which('ffmpeg')
        ffprobe = shutil.which('ffprobe')
        assert ffmpeg and ffprobe
        subprocess.run([ffmpeg, '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=60',
                        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '4.7',
                        '-c:v', 'libx264', '-threads', '2', '-pix_fmt', 'yuv420p', '-c:a', 'aac', str(source_path)], check=True)
        source_sha = hashlib.sha256(source_path.read_bytes()).hexdigest()
        config = root / 'config.json'
        config.write_text(json.dumps({'access_mode': 'restricted', 'listen': f'127.0.0.1:{port}',
            'data_dir': str(state), 'libraries': [str(media)], 'processing': {'ffmpeg': ffmpeg,
            'cache_bytes': 128*1024*1024, 'max_output_bytes': 16*1024*1024, 'timeout_seconds': 60}}))
        log = (root / 'server.log').open('wb')
        command = [str(args.binary.resolve()), '--config', str(config)]
        process = subprocess.Popen(command, stdout=log, stderr=log)
        try:
            wait_for(lambda: request(port, 'GET', '/api/v2/system/health')[0] == 200, seconds=120)
            client = Client(port, (state / 'admin-token').read_text().strip())
            api = client.json
            item = wait_for(lambda: next(iter(api('GET', '/api/v2/catalog/items')['items']), None))
            timeline = api('GET', f'/api/v2/catalog/items/{item["id"]}/timelines')['items'][0]['id']
            original = api('GET', f'/api/v2/catalog/timelines/{timeline}/versions')['items'][0]['files'][0]['file']
            source = next(f for f in api('GET', f'/api/v2/catalog/items/{item["id"]}/files')['items'] if f['id'] == original['file_id'])
            cursor = api('GET', '/api/v2/jobs')['event_cursor']
            profiles = [('h264', 320, 180, {'numerator': 30, 'denominator': 1}), ('hevc', 320, 180, None),
                        ('h264', 640, 360, {'numerator': 30000, 'denominator': 1001}),
                        ('h264', 320, 180, {'numerator': 1, 'denominator': 1}),
                        ('h264', 320, 180, {'numerator': 2, 'denominator': 1})]
            for codec, width, height, fps in profiles:
                profile = {'version': 1, 'codec': codec, 'max_width': width, 'max_height': height,
                           'video_bitrate': 500000, 'audio_bitrate': 96000, 'frame_rate': fps}
                tracks = {'audio_component_id': None, 'subtitle_component_id': None, 'subtitle_policy': 'off',
                          'audio_track_id': None, 'subtitle_track_id': None}
                body = {'source': original, 'timeline_id': timeline, 'recipe_id': 'video_profile',
                        'backend': 'software', 'video_profile': profile, 'tracks': tracks}
                key = {'Idempotency-Key': 'profile-' + str(time.time_ns())}
                job = api('POST', '/api/v2/processing/preparations', body, 202, key)
                assert api('POST', '/api/v2/processing/preparations', body, 202, key)['id'] == job['id']
                def finished():
                    row = api('GET', '/api/v2/jobs/' + job['id'])
                    return row if row['phase'] in ('completed', 'failed', 'cancelled') else None
                row = wait_for(finished, seconds=90)
                assert row['phase'] == 'completed', row
                rendition = next(r for r in api('GET', '/api/v2/processing/renditions')['items'] if r['job_id'] == job['id'])
                assert rendition['status'] == 'ready' and rendition['output']['file_id'] in row['result_ids'], rendition
                output = rendition['output']
                version = next(v for v in api('GET', f'/api/v2/catalog/timelines/{timeline}/versions')['items']
                               if v['origin'] == 'generated' and v['files'][0]['file'] == output)
                plan = api('POST', '/api/v2/playback/plans', {'profile_id': 'default', 'timeline_id': timeline,
                    'version_id': version['id'], 'source': output, 'tracks': tracks,
                    'quality': {'mode': 'auto', 'max_bitrate_bps': None, 'max_height': None,
                                'allow_client_software': False, 'hdr_policy': 'preserve_if_supported'},
                    'client': {'client_id': 'profile-smoke', 'client_build': 'v2', 'demuxe_asset_digest': None,
                               'transports': ['http_range'], 'video_codecs': ['avc1', 'hvc1', 'hev1'],
                               'audio_codecs': ['mp4a'], 'subtitle_modes': ['text'], 'hdr': 'unknown',
                               'max_height': None, 'software_decode': 'unknown', 'cross_origin_isolated': False},
                    'failed_candidate_ids': []})
                assert plan['status'] == 'ready' and plan['operation'] == 'prepared', plan
                delivery = api('POST', '/api/v2/playback/delivery-sessions', {'plan_token': plan['plan_token'], 'start_ms': 0}, 201)
                payload, _ = client.call('GET', delivery['active']['media_url'])
                assert hashlib.sha256(payload).hexdigest() == output['file_revision']
                partial, _ = client.call('GET', delivery['active']['media_url'], expected=206, headers={'Range': 'bytes=0-31'})
                assert partial == payload[:32]
                copy = root / (job['id'] + '.mp4')
                copy.write_bytes(payload)
                probe = json.loads(subprocess.run([ffprobe, '-v', 'error', '-show_streams', '-show_format', '-of', 'json', str(copy)], check=True, capture_output=True).stdout)
                video = next(t for t in probe['streams'] if t['codec_type'] == 'video')
                assert (video['codec_name'], video['width'], video['height']) == (codec, width, height), video
                numerator, denominator = map(int, video['avg_frame_rate'].split('/'))
                wanted = 60 if fps is None else fps['numerator']/fps['denominator']
                assert abs(numerator/denominator-wanted) < .01, video
                assert abs(float(probe['format']['duration'])-source['duration_ms']/1000) <= .5
                assert int(probe['format']['bit_rate']) > 0
                subprocess.run([ffmpeg, '-v', 'error', '-xerror', '-i', str(copy), '-f', 'null', '-'], check=True)
                results.append({'profile': profile, 'job_id': job['id'], 'file_revision': output['file_revision'],
                                'recipe_digest': rendition['recipe_digest'], 'tracks': probe['streams'], 'bytes': len(payload)})
            connection = http.client.HTTPConnection('127.0.0.1', port, timeout=10)
            try:
                connection.request('GET', '/api/v2/events', headers=dict(client.auth, **{'Last-Event-ID': cursor}))
                response = connection.getresponse()
                assert response.status == 200
                seen = set()
                jobs = {r['job_id'] for r in results}
                deadline = time.monotonic() + 30
                while not jobs.issubset(seen):
                    assert time.monotonic() < deadline, ('SSE replay deadline', jobs - seen)
                    raw = response.readline()
                    assert raw, ('SSE closed before replay completed', jobs - seen)
                    line = raw.decode().strip()
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if event.get('resource_type') == 'processing' and event.get('resource_id') in jobs:
                            seen.add(event['resource_id'])
            finally:
                connection.close()
            before = api('GET', '/api/v2/processing/renditions')['items']
            stop(process)
            process = subprocess.Popen(command, stdout=log, stderr=log)
            wait_for(lambda: request(port, 'GET', '/api/v2/system/health')[0] == 200, seconds=120)
            after = api('GET', '/api/v2/processing/renditions')['items']
            assert after == before and len(after) == len(profiles)
            assert len({r['recipe_digest'] for r in after}) == len(profiles)
            assert hashlib.sha256(source_path.read_bytes()).hexdigest() == source_sha
        except BaseException:
            log.flush()
            print((root / 'server.log').read_text(errors='replace')[-16000:])
            raise
        finally:
            stop(process)
            log.close()
    receipt = {'source_sha256': source_sha, 'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
               'encodes': results, 'sse_replay': True, 'restart_preserves_profiles': True, 'range_delivery': True,
               'fully_decoded': True, 'prepared_delivery': True, 'legacy_requests': 0,
               'browser_switching_qualified': False, 'hardware_qualified': False}
    if args.receipt:
        args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()

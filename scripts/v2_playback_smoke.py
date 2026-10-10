"""Real-process /api/v2 playback acceptance: the built server binary over TCP,
a paired restricted device, generated media, real FFprobe scans and FFmpeg HLS.

Covers original byte-range playback, seek, a live conversion with an audio
switch (replan), viewing-session rebinding between deliveries, ordered events,
SIGKILL of the server, recovery (interrupted delivery, durable progress,
outbox drain, continue-watching) and resume. Prints a JSON evidence record.

These are production code paths in a real process; they are not browser or
Electron evidence. Run `cargo build -p playscale` first.
"""
import hashlib, json, os, pathlib, signal, subprocess, sys, tempfile, time, urllib.parse
from smoke import BINARY, free_port, request, wait_for, stop

PLAYER = ['catalog:read', 'playback:request', 'viewing:write']


def ffprobe_json(path):
    out = subprocess.run(['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(path)],
                         check=True, capture_output=True).stdout
    return json.loads(out)['streams']


def main():
    checks = []
    def ok(name):
        checks.append(name); print('PASS', name, file=sys.stderr)
    with tempfile.TemporaryDirectory(prefix='motion-v2-playback-') as directory:
        root = pathlib.Path(directory); media = root / 'media'; media.mkdir(); state = root / 'state'
        film = media / 'film.mp4'
        # H.264 + two AAC tracks (44.1 kHz then 22.05 kHz) so the delivered
        # audio proves which track a conversion selected.
        subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=320x240:rate=24',
                        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=44100',
                        '-f', 'lavfi', '-i', 'sine=frequency=880:sample_rate=22050', '-t', '40',
                        '-map', '0', '-map', '1', '-map', '2', '-c:v', 'libx264', '-preset', 'ultrafast',
                        '-g', '48', '-c:a', 'aac', '-movflags', '+faststart', str(film)], check=True)
        payload = film.read_bytes()
        port = free_port(); log = (root / 'server.log').open('wb')
        command = [str(BINARY), '--access-mode', 'restricted', '--listen', f'127.0.0.1:{port}',
                   '--data-dir', str(state), '--library', str(media)]

        def boot():
            p = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            def ready():
                assert p.poll() is None, ('server exited', p.returncode)
                return request(port, 'GET', '/ready')[0] == 200
            try:
                wait_for(ready, seconds=60)
            except BaseException:
                # Never leave an untracked server behind a failed readiness wait.
                if p.poll() is None:
                    p.terminate()
                    try: p.wait(timeout=10)
                    except subprocess.TimeoutExpired: p.kill(); p.wait()
                raise
            return p

        process = None
        try:
            process = boot()
            operator = {'Authorization': 'Bearer ' + (state / 'admin-token').read_text().strip()}

            def api(method, path, body=None, auth=None, expected=None, extra=None):
                headers = dict(auth or {}); headers.update(extra or {})
                status, h, data = request(port, method, '/api/v2' + path, body, headers)
                parsed = json.loads(data) if data and h.get('content-type', '').startswith(('application/json', 'application/problem+json')) else data
                if expected is not None:
                    assert status == expected, (method, path, status, parsed)
                return status, h, parsed

            def key(name):
                return f'{name}-{time.time_ns()}'

            # Pair a restricted device for the default profile and the library.
            _, _, pairing = api('POST', '/auth/pairings', {'device_name': 'Smoke', 'client_name': 'v2-playback-smoke'}, expected=201)
            api('POST', f"/auth/pairings/{pairing['id']}/approve",
                {'user_code': pairing['user_code'], 'profile_ids': ['default'], 'permissions': PLAYER},
                operator, 200, {'Idempotency-Key': key('approve')})
            _, _, claimed = api('POST', f"/auth/pairings/{pairing['id']}/claim", {'device_code': pairing['device_code']}, expected=200)
            auth = {'Authorization': 'Bearer ' + claimed['access_token']}
            _, _, libraries = api('GET', '/libraries', auth=operator, expected=200)
            library = libraries['items'][0]['id']
            api('PUT', f"/devices/{claimed['device_id']}/policy",
                {'library_ids': [library], 'allow_unrated': True, 'allowed_ratings': [], 'blocked_labels': [], 'permissions': PLAYER},
                operator, 200, {'If-Match': '"r-1"'})

            def first_item():
                status, _, page = api('GET', '/catalog/items', auth=auth)
                return page['items'][0] if status == 200 and page['items'] else None
            item = wait_for(first_item, seconds=60)
            _, _, timelines = api('GET', f"/catalog/items/{item['id']}/timelines", auth=auth, expected=200)
            timeline = timelines['items'][0]['id']
            ok('paired_restricted_device_reads_scanned_catalog')

            def plan(mode, transports, audio=None):
                body = {'profile_id': 'default', 'timeline_id': timeline, 'version_id': None, 'source': None,
                        'tracks': {'audio_component_id': None, 'subtitle_component_id': None, 'subtitle_policy': 'auto',
                                   'audio_track_id': audio, 'subtitle_track_id': None},
                        'quality': {'mode': mode, 'max_bitrate_bps': None, 'max_height': None,
                                    'allow_client_software': True, 'hdr_policy': 'preserve_if_supported'},
                        'client': {'client_id': 'smoke', 'client_build': '1', 'demuxe_asset_digest': None,
                                   'transports': transports, 'video_codecs': ['avc1'], 'audio_codecs': ['mp4a'],
                                   'subtitle_modes': ['text'], 'hdr': 'unknown', 'max_height': None,
                                   'software_decode': 'unknown', 'cross_origin_isolated': False},
                        'failed_candidate_ids': []}
                _, _, p = api('POST', '/playback/plans', body, auth, 200)
                assert p['status'] == 'ready', p
                return p

            def admit(p, start_ms, name):
                _, _, d = api('POST', '/playback/delivery-sessions', {'plan_token': p['plan_token'], 'start_ms': start_ms},
                              auth, 201, {'Idempotency-Key': key(name)})
                return d

            def delivery(d_id):
                return api('GET', f'/playback/delivery-sessions/{d_id}', auth=auth, expected=200)[2]

            # ---- original playback over byte ranges
            p = plan('auto', ['http_range', 'hls'])
            assert (p['transport'], p['operation']) == ('http_range', 'original'), p
            original = admit(p, 0, 'admit-original')
            media_url = original['active']['media_url']
            status, _, body = request(port, 'GET', media_url, headers=dict(auth, Range='bytes=0-65535'))
            assert status == 206 and body == payload[:65536], status
            status, _, body = request(port, 'GET', media_url, headers=auth)
            assert status == 200 and hashlib.sha256(body).hexdigest() == hashlib.sha256(payload).hexdigest()
            ok('original_bytes_and_ranges_match_the_file')

            _, _, session = api('POST', '/playback/viewing-sessions',
                                {'delivery_id': original['id'], 'expected_viewing_revision': '0'},
                                auth, 201, {'Idempotency-Key': key('viewing')})
            sid = session['id']; sequence = [0]
            def event(position, status_name, generation='1', expected=200):
                sequence[0] += 1
                body = {'event_id': f'ev-{sequence[0]}', 'sequence': str(sequence[0]), 'delivery_generation': generation,
                        'position_ms': position, 'status': status_name}
                _, _, ack = api('POST', f'/playback/viewing-sessions/{sid}/events', body, auth, expected)
                return ack, body
            event(4000, 'playing')
            ack, body = event(6000, 'paused')
            _, _, dup = api('POST', f'/playback/viewing-sessions/{sid}/events', body, auth, 200)
            assert dup['duplicate'] is True and dup['session']['sequence'] == ack['session']['sequence']
            ok('ordered_events_and_exact_retry')

            # Seek on the byte route: stage, retry (replayed), activate.
            seek = {'kind': 'seek', 'expected_generation': '1', 'position_ms': 20000}
            k = key('seek')
            _, _, staged = api('POST', f"/playback/delivery-sessions/{original['id']}/changes", seek, auth, 202, {'Idempotency-Key': k})
            _, h, again = api('POST', f"/playback/delivery-sessions/{original['id']}/changes", seek, auth, 202, {'Idempotency-Key': k})
            assert again['pending']['generation'] == staged['pending']['generation'] == '2' and h.get('idempotent-replayed') == 'true'
            _, _, active = api('POST', f"/playback/delivery-sessions/{original['id']}/generations/2/activate",
                               {'expected_active_generation': '1'}, auth, 200, {'Idempotency-Key': key('activate')})
            assert active['active']['requested_start_ms'] == 20000
            ack, _ = event(20500, 'playing', '2')
            ok('byte_route_seek_change_replay_and_activation')

            # ---- a prepared rendition (real processing job) serves Convert by bytes
            status, _, items = request(port, 'GET', '/api/v1/items', headers=operator)
            source = json.loads(items)['items'][0]
            status, _, job = request(port, 'POST', '/api/v1/processing-jobs',
                                     {'source_file_id': source['file_id'], 'source_revision': source['revision'],
                                      'recipe': 'remux_mp4', 'backend': 'software', 'idempotency_key': key('job')}, operator)
            assert status == 201, job
            job = json.loads(job)
            def finished():
                row = json.loads(request(port, 'GET', f"/api/v1/processing-jobs/{job['id']}", headers=operator)[2])
                return row if row['phase'] not in ('queued', 'running', 'cancelling') else None
            assert wait_for(finished, seconds=120)['phase'] == 'completed'
            converted = plan('convert', ['http_range', 'hls'])
            assert (converted['transport'], converted['operation']) == ('http_range', 'prepared'), converted
            assert converted['source']['file_id'] != p['source']['file_id']
            prepared = admit(converted, 0, 'admit-prepared')
            status, _, rendition = request(port, 'GET', prepared['active']['media_url'], headers=auth)
            assert status == 200 and rendition[4:8] == b'ftyp', status
            (root / 'prepared.mp4').write_bytes(rendition)
            assert any(s['codec_type'] == 'video' for s in ffprobe_json(root / 'prepared.mp4'))
            api('DELETE', f"/playback/delivery-sessions/{prepared['id']}", auth=auth, expected=204)
            ok('prepared_rendition_plans_and_serves_bytes_for_convert')

            # ---- live conversion with the second audio track, rebinding the session
            hls = admit(plan('convert', ['hls'], 'a1'), 20500, 'admit-hls')
            def served():
                d = delivery(hls['id'])
                api('POST', f"/playback/delivery-sessions/{hls['id']}/heartbeat",
                    {'active_generation': (d['active'] or d['pending'])['generation']}, auth)
                return d if d['active'] and d['active']['manifest_url'] and d['active']['available_end_ms'] > 20500 + 8000 else None
            ready = wait_for(served, seconds=120)
            master_url = ready['active']['manifest_url']
            status, _, master = request(port, 'GET', master_url, headers=auth)
            assert status == 200 and b'#EXT-X-STREAM-INF' in master, master
            variant = urllib.parse.urljoin(master_url, master.decode().strip().splitlines()[-1])
            status, _, playlist = request(port, 'GET', variant, headers=auth)
            assert status == 200, playlist
            lines = playlist.decode().splitlines()
            init = urllib.parse.urljoin(variant, next(l.split('"')[1] for l in lines if l.startswith('#EXT-X-MAP')))
            segment = urllib.parse.urljoin(variant, next(l for l in lines if l and not l.startswith('#')))
            status, _, init_bytes = request(port, 'GET', init, headers=auth); assert status == 200
            status, _, segment_bytes = request(port, 'GET', segment, headers=auth); assert status == 200
            sample = root / 'sample.mp4'; sample.write_bytes(init_bytes + segment_bytes)
            streams = {s['codec_type']: s for s in ffprobe_json(sample)}
            assert streams['video']['codec_name'] == 'h264' and streams['audio']['codec_name'] == 'aac', streams
            assert streams['audio']['sample_rate'] == '22050', streams['audio']
            # Without credentials the stream is not disclosed.
            assert request(port, 'GET', variant)[0] == 401
            ok('hls_conversion_serves_selected_audio_track')

            _, _, current = api('GET', f'/playback/viewing-sessions/{sid}', auth=auth, expected=200)
            _, _, rebound = api('PUT', f'/playback/viewing-sessions/{sid}/delivery', {'delivery_id': hls['id']},
                                auth, 200, {'If-Match': f'"r-{current["revision"]}"'})
            assert rebound['delivery_id'] == hls['id'] and rebound['sequence'] == current['sequence']
            api('DELETE', f"/playback/delivery-sessions/{original['id']}", auth=auth, expected=204)
            ack, _ = event(26000, 'playing', '1')
            assert ack['session']['delivery_id'] == hls['id']
            ok('viewing_session_rebinds_to_the_conversion')

            # Audio switch on the live delivery: replan to track a0 at the playhead.
            _, _, switched = api('POST', f"/playback/delivery-sessions/{hls['id']}/changes",
                                 {'kind': 'replan', 'expected_generation': '1', 'plan_token': plan('convert', ['hls'], 'a0')['plan_token'], 'position_ms': 26000},
                                 auth, 202, {'Idempotency-Key': key('switch')})
            pending = switched['pending']['generation']
            def pending_ready():
                d = delivery(hls['id'])
                api('POST', f"/playback/delivery-sessions/{hls['id']}/heartbeat", {'active_generation': d['active']['generation']}, auth)
                return d if d['pending'] and d['pending']['status'] == 'ready' else None
            wait_for(pending_ready, seconds=120)
            _, _, switched = api('POST', f"/playback/delivery-sessions/{hls['id']}/generations/{pending}/activate",
                                 {'expected_active_generation': '1'}, auth, 200, {'Idempotency-Key': key('activate-switch')})
            master_url = switched['active']['manifest_url']
            variant = urllib.parse.urljoin(master_url, request(port, 'GET', master_url, headers=auth)[2].decode().strip().splitlines()[-1])
            lines = request(port, 'GET', variant, headers=auth)[2].decode().splitlines()
            init = urllib.parse.urljoin(variant, next(l.split('"')[1] for l in lines if l.startswith('#EXT-X-MAP')))
            segment = urllib.parse.urljoin(variant, next(l for l in lines if l and not l.startswith('#')))
            sample.write_bytes(request(port, 'GET', init, headers=auth)[2] + request(port, 'GET', segment, headers=auth)[2])
            audio = next(s for s in ffprobe_json(sample) if s['codec_type'] == 'audio')
            assert audio['sample_rate'] == '44100', audio
            ack, queued = event(27000, 'playing', pending)
            ok('live_audio_switch_by_replan_and_activation')

            # ---- crash: SIGKILL, restart, recovery
            process.send_signal(signal.SIGKILL); process.wait(); process = None
            process = boot()
            # Durable progress first, before any write could restore it.
            acknowledged = ack['viewing']
            _, _, viewing = api('GET', f'/profiles/default/timelines/{timeline}/viewing', auth=auth, expected=200)
            assert (viewing['position_ms'], viewing['revision'], viewing['session_id']) == \
                (27000, acknowledged['revision'], sid), (viewing, acknowledged)
            _, _, persisted = api('GET', f'/playback/viewing-sessions/{sid}', auth=auth, expected=200)
            assert persisted['sequence'] == ack['session']['sequence'] and persisted['position_ms'] == 27000
            assert delivery(hls['id'])['status'] == 'interrupted'
            api('POST', f"/playback/delivery-sessions/{hls['id']}/heartbeat", {'active_generation': pending}, auth, 409)
            api('DELETE', f"/playback/delivery-sessions/{hls['id']}", auth=auth, expected=204)
            # The acknowledged event replays; a queued one drains after the crash.
            _, _, dup = api('POST', f'/playback/viewing-sessions/{sid}/events', queued, auth, 200)
            assert dup['duplicate'] is True
            ack, _ = event(27500, 'paused', pending)
            _, _, viewing = api('GET', f'/profiles/default/timelines/{timeline}/viewing', auth=auth, expected=200)
            assert viewing['position_ms'] == 27500 and viewing['session_id'] == sid, viewing
            _, _, cont = api('GET', '/profiles/default/continue-watching', auth=auth, expected=200)
            assert cont['items'][0]['timeline']['id'] == timeline and cont['items'][0]['viewing']['position_ms'] == 27500
            ok('sigkill_recovery_interrupts_delivery_keeps_progress_and_drains_outbox')

            # Resume on a new delivery from the saved position.
            resumed = admit(plan('auto', ['http_range']), viewing['position_ms'], 'admit-resume')
            _, _, fresh = api('POST', '/playback/viewing-sessions',
                              {'delivery_id': resumed['id'], 'expected_viewing_revision': viewing['revision']},
                              auth, 201, {'Idempotency-Key': key('viewing-resume')})
            assert fresh['position_ms'] == 27500 and resumed['active']['requested_start_ms'] == 27500
            _, _, old = api('GET', f'/playback/viewing-sessions/{sid}', auth=auth, expected=200)
            assert old['status'] == 'superseded'
            ok('resume_supersedes_the_previous_session')
        finally:
            if process is not None:
                stop(process)
            log.close()
    print(json.dumps({'checks': checks, 'passed': len(checks),
                      'binary_sha256': hashlib.sha256(BINARY.read_bytes()).hexdigest(),
                      'fixture_sha256': hashlib.sha256(payload).hexdigest()}, indent=2))


if __name__ == '__main__':
    main()

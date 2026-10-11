"""Real v2 HTTP viewing authority: retries, restart, supersession and preferences.
Positive release-order Next provisioning is a separate catalog API gate.
"""
import json
import pathlib
import subprocess
import tempfile
from smoke import BINARY, free_port, request, wait_for, stop
from v2_smoke_support import Client


def main():
    checks = []
    with tempfile.TemporaryDirectory(prefix='motion-viewing-') as directory:
        base = pathlib.Path(directory)
        media = base / 'media'
        media.mkdir()
        subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=160x90:rate=12', '-t', '3', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', str(media / 'Episode.mp4')], check=True)
        port = free_port()
        command = [str(BINARY), '--listen', f'127.0.0.1:{port}', '--data-dir', str(base / 'state'), '--library', str(media)]
        with (base / 'server.log').open('wb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            def ready():
                wait_for(lambda: request(port, 'GET', '/api/v2/system/health')[0] == 200, 120)
            try:
                ready()
                client = Client(port, (base / 'state/admin-token').read_text().strip())
                api = client.json
                item = wait_for(lambda: next(iter(api('GET', '/api/v2/catalog/items')['items']), None))
                timeline = api('GET', f'/api/v2/catalog/items/{item["id"]}/timelines')['items'][0]['id']
                view = f'/api/v2/profiles/default/timelines/{timeline}/viewing'
                sessions = '/api/v2/playback/viewing-sessions'
                epoch = client.viewing_epoch()  # Retain the observation across restart/replay.
                def start():
                    delivery = client.original_delivery(timeline)
                    session = api('POST', sessions, {'delivery_id': delivery['id'],
                        'expected_viewing_revision': api('GET', view)['revision']}, 201, epoch)
                    return delivery, session
                delivery, first = start()
                first_path = sessions + '/' + first['id']
                event = {'event_id': 'first-progress', 'sequence': '1', 'delivery_generation': delivery['active']['generation'], 'position_ms': 1000, 'status': 'playing'}
                ack = api('POST', first_path + '/events', event, headers=epoch)
                duplicate = api('POST', first_path + '/events', event, headers=epoch)
                assert duplicate['duplicate'] and duplicate['session']['sequence'] == '1'
                assert api('GET', view)['revision'] == ack['viewing']['revision']
                api('POST', first_path + '/events', dict(event, position_ms=2000), 409, epoch)
                assert len(api('GET', '/api/v2/profiles/default/continue-watching')['items']) == 1
                checks.append('ordered_progress_exact_retry_conflict_and_continue_watching')
                prefs_path = '/api/v2/profiles/default/preferences'
                prefs = {'audio_languages': ['en-US', 'ja'], 'subtitle_languages': ['en'], 'subtitle_mode': 'foreign_audio',
                         'quality_mode': 'original', 'allow_client_software_decode': False, 'autoplay': False, 'completion_percent': 90}
                api('PUT', prefs_path, prefs, headers={'If-Match': client.etag(prefs_path)})
                stop(process)
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                ready()
                assert client.viewing_epoch() == epoch
                assert api('GET', first_path)['sequence'] == '1'
                assert api('GET', prefs_path)['audio_languages'] == ['en-us', 'ja']
                # Durable authority accepts the queued event after delivery interruption.
                api('POST', first_path + '/events', dict(event, event_id='queued-after-restart', sequence='2', position_ms=500, status='paused'), headers=epoch)
                assert api('GET', view)['position_ms'] == 500
                checks.append('authority_queued_progress_and_preferences_survive_restart')
                delivery, second = start()
                api('POST', first_path + '/events', dict(event, event_id='stale-authority', sequence='3', position_ms=2000), 409, epoch)
                assert api('GET', first_path)['status'] == 'superseded'
                checks.append('superseded_session_cannot_publish_progress')
                api('POST', sessions + '/' + second['id'] + '/events', dict(event, event_id='completed',
                    delivery_generation=delivery['active']['generation'], position_ms=3000, status='ended'), headers=epoch)
                assert api('GET', view)['watched'] is True
                assert api('GET', '/api/v2/profiles/default/continue-watching')['items'] == []
                stale = client.etag(view)
                api('PUT', view, {'manual_watched': False, 'reset_position': False}, headers=dict(epoch, **{'If-Match': stale}))
                assert api('GET', view)['watched'] is False
                api('PUT', view, {'manual_watched': None, 'reset_position': False}, 412, dict(epoch, **{'If-Match': stale}))
                api('PUT', view, {'manual_watched': None, 'reset_position': False}, headers=dict(epoch, **{'If-Match': client.etag(view)}))
                assert api('GET', view)['watched'] is True
                stop(process)
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                ready()
                persisted = api('GET', view)
                assert persisted['watched'] is True and persisted['manual_watched'] is None
                checks.append('automatic_completion_revision_fenced_manual_override_and_restart')
                assert api('GET', f'/api/v2/profiles/default/timelines/{timeline}/next')['items'] == []
                checks.append('unplaced_timeline_has_no_inferred_next')
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks, 'api_version': 'v2',
                      'remaining_gates': ['positive_release_order_next_requires_catalog_provisioning']}, indent=2))


if __name__ == '__main__':
    main()

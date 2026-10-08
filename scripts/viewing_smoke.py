"""Real HTTP viewing-state acceptance: sessions, restart, ordering and preferences."""
import json
import pathlib
import subprocess
import tempfile
from smoke import BINARY, free_port, request, wait_for, stop


def main():
    checks = []
    with tempfile.TemporaryDirectory(prefix='playscale-viewing-') as directory:
        base = pathlib.Path(directory)
        media = base / 'media'
        media.mkdir()
        subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=160x90:rate=12', '-t', '3', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', str(media / 'Episode.mp4')], check=True)
        port = free_port()
        command = [str(BINARY), '--listen', f'127.0.0.1:{port}', '--data-dir', str(base / 'state'), '--library', str(media)]
        with (base / 'server.log').open('wb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: request(port, 'GET', '/health')[0] == 200)
                auth = {'Authorization': 'Bearer ' + (base / 'state/admin-token').read_text().strip()}
                def api(method, path, body=None, expected=200):
                    status, _, payload = request(port, method, '/api/v1' + path, body, auth)
                    assert status == expected, (method, path, status, payload)
                    return json.loads(payload)
                def discovered():
                    rows = api('GET', '/items')['items']
                    return rows[0] if rows else None
                item = wait_for(discovered)
                profile = '/profiles/default'
                view = f'{profile}/viewing/{item["id"]}'
                sessions = profile + '/playback-sessions'
                start = {'item_id': item['id'], 'file_id': item['file_id'], 'file_revision': item['revision'], 'expected_revision': 0}
                first = api('POST', sessions, start, 201)
                first_path = sessions + '/' + first['id']
                event = {'sequence': 1, 'position_seconds': 1, 'status': 'playing'}
                api('PUT', first_path, event)
                revision = api('GET', view)['revision']
                api('PUT', first_path, event)
                assert api('GET', view)['revision'] == revision
                api('PUT', first_path, dict(event, position_seconds=2), 409)
                assert api('GET', profile + '/continue-watching')['total'] == 1
                checks.append('ordered_progress_retry_and_continue_watching')
                prefs = {'audio_languages': ['en-US', 'ja'], 'subtitle_languages': ['en'], 'subtitle_mode': 'foreign_audio', 'quality': 'original'}
                api('PUT', profile + '/playback-preferences', {'expected_revision': 0, 'preferences': prefs})
                stop(process)
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                wait_for(lambda: request(port, 'GET', '/health')[0] == 200)
                assert api('GET', first_path)['sequence'] == 1
                assert api('GET', profile + '/playback-preferences')['preferences']['audio_languages'] == ['en-us', 'ja']
                api('PUT', first_path, {'sequence': 2, 'position_seconds': .5, 'status': 'paused'})
                assert api('GET', view)['position_seconds'] == .5
                checks.append('sessions_progress_and_preferences_survive_restart')
                start['expected_revision'] = api('GET', view)['revision']
                second = api('POST', sessions, start, 201)
                api('PUT', first_path, {'sequence': 3, 'position_seconds': 2, 'status': 'playing'}, 409)
                api('PUT', f'{profile}/progress/{item["id"]}', {'position_seconds': 2}, 409)
                checks.append('superseded_session_and_legacy_write_rejection')
                api('PUT', sessions + '/' + second['id'], {'sequence': 1, 'position_seconds': 3, 'status': 'ended'})
                assert api('GET', view)['watched'] is True
                assert api('GET', profile + '/continue-watching')['total'] == 0
                revision = api('GET', view)['revision']
                api('PUT', view, {'expected_revision': revision, 'watched': False})
                assert api('GET', view)['watched'] is False
                api('PUT', view, {'expected_revision': revision + 1, 'watched': None})
                assert api('GET', view)['watched'] is True
                stop(process)
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                wait_for(lambda: request(port, 'GET', '/health')[0] == 200)
                persisted = api('GET', view)
                assert persisted['watched'] is True and persisted['automatic_watched'] is True and persisted['manual_watched'] is None
                assert api('GET', sessions + '/' + second['id'])['status'] == 'invalidated'
                checks.append('automatic_completion_and_manual_override')
                show = api('POST', '/catalog/items', {'title': 'Show', 'media_type': 'series'}, 201)
                season = api('POST', '/catalog/items', {'title': 'Season', 'media_type': 'season', 'parent_id': show['id'], 'number': 1}, 201)
                api('PUT', f'/items/{item["id"]}/structure', {'expected_revision': 0, 'media_type': 'episode', 'parent_id': season['id'], 'number': 1})
                following = api('POST', '/catalog/items', {'title': 'Next', 'media_type': 'episode', 'parent_id': season['id'], 'number': 2}, 201)
                assert api('GET', f'{profile}/next-episode/{item["id"]}')['next']['item_id'] == following['id']
                assert api('GET', f'{profile}/next-episode/{following["id"]}')['next'] is None
                checks.append('next_episode_order_and_end_of_series')
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks}, indent=2))


if __name__ == '__main__':
    main()

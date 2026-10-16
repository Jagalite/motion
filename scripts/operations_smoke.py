"""Verify configuration, readiness, live snapshots, safe restore and SIGTERM handling."""
import json
import pathlib
import shutil
import signal
import subprocess
import tempfile
import uuid
from smoke import BINARY, free_port, request, wait_for, stop
from backup import backup, restore


def main():
    checks = []
    with tempfile.TemporaryDirectory(prefix='playscale-operations-') as directory:
        root = pathlib.Path(directory)
        config = root / 'config.json'
        port = free_port()
        config.write_text(json.dumps({'access_mode': 'restricted', 'listen': f'127.0.0.1:{port}', 'data_dir': 'state', 'libraries': [], 'demuxe_dir': 'missing-assets', 'ffprobe': shutil.which('ffprobe')}))
        effective = json.loads(subprocess.check_output([str(BINARY), '--config', str(config), '--check-config'], text=True))
        assert pathlib.Path(effective['data_dir']).name == 'state' and pathlib.Path(effective['data_dir']).is_absolute()
        assert not (root / 'state').exists()
        checks.append('configuration_validation_without_data_mutation')
        with (root / 'server.log').open('wb') as log:
            command = [str(BINARY), '--config', str(config)]
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: request(port, 'GET', '/ready')[0] == 200)
                auth = {'Authorization': 'Bearer ' + (root / 'state/admin-token').read_text().strip()}
                def api(method, path, body=None, expected=200, headers=None, with_headers=False):
                    extra = dict(headers or {})
                    if method == 'POST':
                        extra.setdefault('Idempotency-Key', str(uuid.uuid4()))
                    status, response_headers, data = request(port, method, '/api/v2' + path, body, dict(auth, **extra))
                    assert status == expected, (status, data)
                    return (json.loads(data), response_headers) if with_headers else json.loads(data)
                assert request(port, 'GET', '/api/v2/admin/diagnostics')[0] == 401
                diag = api('GET', '/admin/diagnostics')
                health = api('GET', '/system/health')
                assert health['status'] == 'ok' and diag['server_epoch'] == health['server_epoch']
                assert all(int(diag[key]) >= 0 for key in ('uptime_seconds', 'active_deliveries', 'queued_jobs', 'running_jobs', 'db_busy_count'))
                assert isinstance(diag['worker_errors'], list)
                checks.append('readiness_and_authenticated_diagnostics')
                item, item_headers = api('POST', '/catalog/items', {'title': 'Backed up movie', 'kind': 'movie', 'library_ids': []}, 201, with_headers=True)
                edition = api('POST', f'/catalog/items/{item["id"]}/editions',
                              {'label': 'Default', 'kind': 'default'}, 201,
                              {'If-Match': item_headers['etag']})
                identity = api('GET', '/system/capabilities')
                viewing_epoch = identity['server_id'] + ':' + identity['restore_epoch']
                timeline = edition['timeline_ids'][0]
                viewing_path = f'/profiles/default/timelines/{timeline}/viewing'
                viewing, viewing_headers = api('GET', viewing_path, with_headers=True)
                api('PUT', viewing_path, {'manual_watched': True, 'reset_position': False},
                    headers={'If-Match': viewing_headers['etag'], 'X-Motion-Viewing-Epoch': viewing_epoch})
                manifest = backup(root / 'state', root / 'snapshot')
                assert manifest['schema_versions']
                # This later write must not appear in the already completed snapshot.
                api('POST', '/catalog/items', {'title': 'After snapshot', 'kind': 'movie', 'library_ids': []}, 201)
                restored = root / 'restored'
                restore(root / 'snapshot', restored)
                assert not (restored / 'admin-token').exists()
                try:
                    restore(root / 'snapshot', restored)
                    raise AssertionError('Existing destination was accepted')
                except ValueError:
                    pass
                corrupt = root / 'corrupt'
                shutil.copytree(root / 'snapshot', corrupt)
                with (corrupt / 'playscale.sqlite3').open('ab') as file:
                    file.write(b'changed')
                try:
                    restore(corrupt, root / 'bad-restore')
                    raise AssertionError('Corrupt snapshot was accepted')
                except ValueError:
                    pass
                assert not (root / 'bad-restore').exists()
                checks.append('live_snapshot_integrity_and_non_overwriting_restore')
                process.send_signal(signal.SIGTERM)
                process.wait(timeout=15)
                assert process.returncode == 0
                checks.append('graceful_sigterm_exit')
                process = subprocess.Popen(command + ['--data-dir', str(restored)], stdout=log, stderr=subprocess.STDOUT)
                wait_for(lambda: request(port, 'GET', '/ready')[0] == 200)
                old_auth = auth['Authorization']
                auth['Authorization'] = 'Bearer ' + (restored / 'admin-token').read_text().strip()
                assert auth['Authorization'] != old_auth
                assert request(port, 'GET', '/api/v2/admin/diagnostics', headers={'Authorization': old_auth})[0] == 401
                catalog = api('GET', '/catalog/items')
                assert [row['id'] for row in catalog['items']] == [item['id']]
                assert catalog['next_cursor'] is None
                assert api('GET', viewing_path)['watched'] is True
                restored_identity = api('GET', '/system/capabilities')
                assert restored_identity['server_id'] == identity['server_id']
                assert restored_identity['restore_epoch'] != identity['restore_epoch']
                _, restored_headers = api('GET', viewing_path, with_headers=True)
                stale = api('PUT', viewing_path, {'manual_watched': False, 'reset_position': False},
                            expected=409, headers={'If-Match': restored_headers['etag'],
                                                   'X-Motion-Viewing-Epoch': viewing_epoch})
                assert stale['code'] == 'viewing_epoch_changed', stale
                assert api('GET', viewing_path)['watched'] is True
                api('PUT', viewing_path, {'manual_watched': False, 'reset_position': False},
                    headers={'If-Match': restored_headers['etag'], 'X-Motion-Viewing-Epoch':
                             restored_identity['server_id'] + ':' + restored_identity['restore_epoch']})
                assert api('GET', viewing_path)['watched'] is False
                checks.append('restore_fences_observed_history_writes_until_reconciliation')
                checks.append('restored_server_state_and_fresh_credential')
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks}, indent=2))


if __name__ == '__main__':
    main()

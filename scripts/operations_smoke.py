"""Verify configuration, readiness, live snapshots, safe restore and SIGTERM handling."""
import json
import pathlib
import shutil
import signal
import subprocess
import tempfile
from smoke import BINARY, free_port, request, wait_for, stop
from backup import backup, restore


def main():
    checks = []
    with tempfile.TemporaryDirectory(prefix='playscale-operations-') as directory:
        root = pathlib.Path(directory)
        config = root / 'config.json'
        port = free_port()
        config.write_text(json.dumps({'access_mode': 'trusted_household', 'listen': f'127.0.0.1:{port}', 'data_dir': 'state', 'libraries': [], 'demuxe_dir': 'missing-assets', 'ffprobe': shutil.which('ffprobe')}))
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
                def api(method, path, body=None, expected=200):
                    status, _, data = request(port, method, '/api/v1' + path, body, auth)
                    assert status == expected, (status, data)
                    return json.loads(data)
                assert request(port, 'GET', '/api/v1/admin/diagnostics')[0] == 401
                diag = api('GET', '/admin/diagnostics')
                assert diag['readiness']['ready'] is True and diag['ffprobe_available'] is True and diag['demuxe_present_at_startup'] is False
                checks.append('readiness_and_authenticated_diagnostics')
                item = api('POST', '/catalog/items', {'title': 'Backed up movie', 'media_type': 'movie'}, 201)
                api('PUT', f'/profiles/default/viewing/{item["id"]}', {'expected_revision': 0, 'watched': True})
                manifest = backup(root / 'state', root / 'snapshot')
                assert manifest['schema_versions']
                # This later write must not appear in the already completed snapshot.
                api('POST', '/catalog/items', {'title': 'After snapshot', 'media_type': 'movie'}, 201)
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
                assert api('GET', '/catalog/items')['total'] == 1
                assert api('GET', f'/profiles/default/viewing/{item["id"]}')['watched'] is True
                checks.append('restored_server_state_and_fresh_credential')
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks}, indent=2))


if __name__ == '__main__':
    main()

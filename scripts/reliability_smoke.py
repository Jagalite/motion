"""Bounded inventory/recovery test; fake probe data is not codec qualification."""
import concurrent.futures
import json
import pathlib
import shutil
import sqlite3
import subprocess
import tempfile
import time
from smoke import BINARY, free_port, request, wait_for, stop
from backup import restore


def main():
    checks = []
    with tempfile.TemporaryDirectory(prefix='playscale-reliability-') as directory:
        root = pathlib.Path(directory)
        media = root / 'media'
        media.mkdir()
        count = 1000
        for n in range(count):
            (media / f'{n:05}.mp4').write_bytes(f'inventory fixture {n}'.encode())
        probe = root / 'probe'
        probe.write_text('#!/bin/sh\nprintf \'%s\\n\' \'{"format":{"duration":"100"},"streams":[]}\'\n')
        probe.chmod(0o700)
        port = free_port()
        state = root / 'state'
        config = root / 'config.json'
        config.write_text(json.dumps({'listen': f'127.0.0.1:{port}', 'data_dir': str(state),
            'ffprobe': str(probe), 'libraries': [str(root / 'unavailable')],
            'storage': {'backup_interval_seconds': 60, 'backups_keep': 2, 'min_free_bytes': 0}}))
        command = [str(BINARY), '--config', str(config)]
        with (root / 'launcher.log').open('wb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: request(port, 'GET', '/ready')[0] == 200)
                auth = {'Authorization': 'Bearer ' + (state / 'admin-token').read_text().strip()}
                def api(method, path, body=None, expected=200):
                    status, _, raw = request(port, method, '/api/v1' + path, body, auth)
                    assert status == expected, (path, status, raw)
                    return json.loads(raw) if raw else None
                def unavailable_reported():
                    return any(s['name'] == 'configured_libraries' and s['error'] for s in api('GET', '/admin/storage')['state'])
                wait_for(unavailable_reported)
                checks.append('unavailable_configured_root_does_not_block_readiness')
                library = api('POST', '/libraries', {'name': 'Inventory', 'root': str(media)}, 201)
                def scan(full=False):
                    started = time.monotonic()
                    job = api('POST', f'/libraries/{library["id"]}/scans' + ('?full=true' if full else ''), expected=202)
                    result = wait_for(lambda: (j if (j := api('GET', '/jobs/' + job['id']))['phase'] not in ('queued','running','cancelling') else None), 180)
                    assert result['phase'] == 'completed', result
                    return result, round(time.monotonic() - started, 3)
                first, first_seconds = scan()
                assert first['inspected_files'] == count, first
                second, incremental_seconds = scan()
                assert second['reused_files'] == count and second['inspected_files'] == 0, second
                checks.append('1000_file_inventory_and_incremental_reuse')
                item = api('GET', '/items?limit=1')['items'][0]
                progress = '/profiles/default/progress/' + item['id']
                latencies = []
                def writes():
                    for n in range(30):
                        t = time.monotonic()
                        api('PUT', progress, {'position_seconds': n})
                        latencies.append(time.monotonic() - t)
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    writing = pool.submit(writes)
                    full, full_seconds = scan(True)
                    writing.result()
                assert full['inspected_files'] == count and full['reused_files'] == 0
                assert max(latencies) < 5, latencies
                checks.append('full_scan_and_concurrent_viewing_writes')
                # Scheduled snapshot worker must produce a compatible restore artifact.
                snapshots = wait_for(lambda: [p for p in (state / 'backups').glob('auto-*/manifest.json') if not p.parent.name.endswith('.partial')], 45)
                restored = root / 'restored'
                restore(snapshots[0].parent, restored)
                assert not (restored / 'admin-token').exists()
                with sqlite3.connect(restored / 'playscale.sqlite3') as db:
                    assert db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
                checks.append('scheduled_snapshot_and_existing_restore_tool')
                for _ in range(3): api('POST', '/admin/storage', expected=201)
                assert len([p for p in (state / 'backups').glob('auto-*/manifest.json') if not p.parent.name.endswith('.partial')]) == 2
                checks.append('automatic_snapshot_retention')
                # Mount removal must fail the scan without publishing an empty catalog.
                media.rename(root / 'offline')
                job = api('POST', f'/libraries/{library["id"]}/scans', expected=202)
                result = wait_for(lambda: (j if (j := api('GET', '/jobs/' + job['id']))['phase'] == 'failed' else None))
                assert api('GET', '/admin/libraries')[0]['available_files'] == count
                checks.append('disconnected_root_preserves_catalog')
                stop(process)
                # Disk pressure is injected via reserve, never by filling the host disk.
                settings = json.loads(config.read_text())
                settings['storage']['min_free_bytes'] = 2**63 - 1
                config.write_text(json.dumps(settings))
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                wait_for(lambda: request(port, 'GET', '/ready')[0] == 200)
                api('POST', '/admin/storage', expected=503)
                api('PUT', progress, {'position_seconds': 31})
                assert api('GET', progress)['position_seconds'] == 31
                checks.append('disk_reserve_failure_keeps_server_and_viewing_writes_live')
                assert (state / 'logs/server.log').exists()
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks, 'inventory_files': count,
        'scan_seconds': {'first': first_seconds, 'incremental': incremental_seconds, 'full': full_seconds},
        'max_viewing_write_seconds': round(max(latencies), 3)}, indent=2))


if __name__ == '__main__':
    main()

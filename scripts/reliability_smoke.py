"""Bounded inventory/recovery test; fake probe data is not codec qualification."""
import concurrent.futures
import json
import pathlib
import shutil
import sqlite3
import subprocess
import tempfile
import time
import sys
from smoke import BINARY, free_port, request, wait_for, stop
from backup import restore
from v2_smoke_support import Client


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
        config.write_text(json.dumps({'access_mode': 'restricted', 'listen': f'127.0.0.1:{port}', 'data_dir': str(state),
            'ffprobe': str(probe), 'libraries': [str(root / 'unavailable')],
            'storage': {'backup_interval_seconds': 60, 'backups_keep': 2, 'min_free_bytes': 0}}))
        command = [str(BINARY), '--config', str(config)]
        with (root / 'launcher.log').open('wb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: request(port, 'GET', '/ready')[0] == 200)
                auth = {'Authorization': 'Bearer ' + (state / 'admin-token').read_text().strip()}
                client=Client(port,auth['Authorization'][7:]); api=client.json
                # A bad configured mount must not prevent health/storage inspection.
                report=api('GET','/api/v2/admin/storage')
                assert int(report['database_bytes'])>0
                checks.append('unavailable_configured_root_does_not_block_readiness')
                source=api('POST','/api/v2/sources',{'name':'Inventory','root_path':str(media),'exclusions':[]},201)
                library=api('POST','/api/v2/libraries',{'name':'Inventory','kind':'mixed','language':'en','source_ids':[source['id']]},201)
                def scan(full=False):
                    started=time.monotonic()
                    demand=api('POST',f'/api/v2/libraries/{library["id"]}/scans',{'mode':'verify' if full else 'incremental','require_complete':True},202)
                    result=wait_for(lambda: (r if (r:=api('GET','/api/v2/scans/'+demand['id']))['status'] not in ('queued','running') else None),180)
                    assert result['status']=='complete' and result['complete'],result
                    # Adapter counters remain observable in the isolated database;
                    # the public demand identifies the exact jobs to inspect.
                    with sqlite3.connect(state/'playscale.sqlite3') as db:
                        metrics=[db.execute('SELECT inspected_files,reused_files FROM jobs WHERE id=?',(job,)).fetchone() for job in result['job_ids']]
                    return {'inspected_files':sum(r[0] for r in metrics),'reused_files':sum(r[1] for r in metrics)},round(time.monotonic()-started,3)
                first, first_seconds = scan()
                assert first['inspected_files'] == count, first
                second, incremental_seconds = scan()
                assert second['reused_files'] == count and second['inspected_files'] == 0, second
                checks.append('1000_file_inventory_and_incremental_reuse')
                item=api('GET','/api/v2/catalog/items?limit=1')['items'][0]
                timeline=api('GET',f'/api/v2/catalog/items/{item["id"]}/timelines')['items'][0]['id']
                progress=f'/api/v2/profiles/default/timelines/{timeline}/viewing'
                epoch=client.viewing_epoch()
                def write_view(watched):
                    return api('PUT',progress,{'manual_watched':watched,'reset_position':False},headers=dict(epoch,**{'If-Match':client.etag(progress)}))
                latencies=[]
                def writes():
                    for n in range(30):
                        t=time.monotonic();write_view(bool(n%2));latencies.append(time.monotonic()-t)
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
                def backup_request():
                    backup=api('POST','/api/v2/admin/backups',{'include_generated_media':False},202)
                    latest = None
                    def terminal():
                        nonlocal latest
                        latest = api('GET', '/api/v2/jobs/' + backup['job_id'])
                        return latest if latest['phase'] in ('completed', 'failed', 'cancelled') else None
                    try:
                        return wait_for(terminal, 45)
                    except Exception as error:
                        raise AssertionError({'backup': backup, 'last_job': latest}) from error
                for _ in range(3): assert backup_request()['phase']=='completed'
                wait_for(lambda:sum(b['status']=='ready' for b in api('GET','/api/v2/admin/backups')['items'])==2,40)
                wait_for(lambda:len(list((state/'backups').glob('request-*/manifest.json')))==2,40)
                checks.append('durable_requested_snapshot_retention')
                # Mount removal must fail the scan without publishing an empty catalog.
                media.rename(root/'offline')
                demand=api('POST',f'/api/v2/libraries/{library["id"]}/scans',{'mode':'verify','require_complete':True},202)
                result=wait_for(lambda:(r if (r:=api('GET','/api/v2/scans/'+demand['id']))['status'] not in ('queued','running') else None),45)
                assert result['status'] in ('unavailable','failed','partial') and not result['complete'],result
                with sqlite3.connect(state/'playscale.sqlite3') as db:
                    assert db.execute('SELECT count(*) FROM media_files WHERE available=1 AND generated=0').fetchone()[0]==count
                checks.append('disconnected_root_preserves_catalog')
                stop(process)
                # Disk pressure is injected via reserve, never by filling the host disk.
                settings = json.loads(config.read_text())
                settings['storage']['min_free_bytes'] = 2**63 - 1
                config.write_text(json.dumps(settings))
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                wait_for(lambda: request(port, 'GET', '/ready')[0] == 200)
                assert backup_request()['phase']=='failed'
                write_view(True)
                assert api('GET',progress)['manual_watched'] is True
                checks.append('disk_reserve_failure_keeps_server_and_viewing_writes_live')
                assert (state / 'logs/server.log').exists()
            except BaseException:
                print(json.dumps({'completed_checks': checks}, indent=2), file=sys.stderr)
                diagnostic = state / 'logs/server.log'
                if diagnostic.exists():
                    print(diagnostic.read_text()[-12000:], file=sys.stderr)
                raise
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks, 'inventory_files': count,
        'scan_seconds': {'first': first_seconds, 'incremental': incremental_seconds, 'full': full_seconds},
        'max_viewing_write_seconds': round(max(latencies), 3)}, indent=2))


if __name__ == '__main__':
    main()

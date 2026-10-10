"""Real FFmpeg, durable jobs, SSE replay, scan schedules and owned-cache acceptance."""
import json, pathlib, subprocess, tempfile, hashlib, sys, os, time, sqlite3, http.client
from smoke import BINARY, free_port, request, wait_for, stop
from backup import backup, restore


def main():
    checks=[]
    with tempfile.TemporaryDirectory(prefix='playscale-processing-') as directory:
        root=pathlib.Path(directory); media=root/'media'; media.mkdir()
        import shutil
        ffmpeg=shutil.which('ffmpeg'); assert ffmpeg
        subprocess.run([ffmpeg,'-v','error','-f','lavfi','-i','testsrc2=size=320x180:rate=24','-f','lavfi','-i','sine=frequency=440:sample_rate=48000','-t','3','-c:v','libx264','-threads','2','-pix_fmt','yuv420p','-c:a','aac',str(media/'clip.mp4')],check=True)
        original=hashlib.sha256((media/'clip.mp4').read_bytes()).hexdigest()
        gate=root/'gate'; pidfile=root/'encoder-pid'
        wrapper=root/'ffmpeg-wrapper'
        wrapper.write_text('#!'+sys.executable+'\nimport os,time,pathlib\npathlib.Path('+repr(str(pidfile))+').write_text(str(os.getpid()))\nwhile pathlib.Path('+repr(str(gate))+').exists(): time.sleep(.05)\nos.execv('+repr(ffmpeg)+',['+repr(ffmpeg)+']+__import__("sys").argv[1:])\n');wrapper.chmod(0o700)
        normal_wrapper=wrapper.read_text()
        port=free_port();state=root/'state';config=root/'config.json'
        config.write_text(json.dumps({'access_mode':'trusted_household','listen':f'127.0.0.1:{port}','data_dir':str(state),'libraries':[str(media)],'processing':{'ffmpeg':str(wrapper),'cache_bytes':32*1024*1024,'max_output_bytes':4*1024*1024,'timeout_seconds':20,'retention_seconds':60}}))
        log=(root/'log').open('wb'); process=None
        def boot(data=None):
            p=subprocess.Popen([str(BINARY),'--config',str(config)]+(['--data-dir',str(data)] if data else []),stdout=log,stderr=log)
            def ready():
                assert p.poll() is None, ('server exited before readiness', p.returncode)
                return request(port,'GET','/ready')[0]==200
            try:
                wait_for(ready)
                return p
            except BaseException:
                # A failed boot must not leave an untracked server behind, and
                # restore failures need diagnostics from the restored root.
                if p.poll() is None:
                    p.terminate()
                    try: p.wait(timeout=5)
                    except subprocess.TimeoutExpired: p.kill();p.wait()
                diagnostic=(data or state)/'logs/server.log'
                if diagnostic.exists(): print(diagnostic.read_text()[-4000:],file=sys.stderr)
                raise
        try:
            process=boot();auth={'Authorization':'Bearer '+(state/'admin-token').read_text().strip()}
            def api(method,path,body=None,expected=200,protected=True):
                status,_,data=request(port,method,'/api/v1'+path,body,auth if protected else {})
                assert status==expected,(path,status,data.decode());return json.loads(data)
            def scanned():
                page=api('GET','/items');return page['items'][0] if page['items'] else None
            source=wait_for(scanned);library=source['library_id']
            def submit(recipe='h264720p',backend='software',key=None):
                return api('POST','/processing-jobs',{'source_file_id':source['file_id'],'source_revision':source['revision'],'recipe':recipe,'backend':backend,'idempotency_key':key or str(time.time_ns())},201)
            def terminal(job):
                def done():
                    row=api('GET','/processing-jobs/'+job['id']);return row if row['phase'] not in ['queued','running','cancelling'] else None
                return wait_for(done,seconds=35)
            base={'source_file_id':source['file_id'],'source_revision':source['revision'],'recipe':'remux_mp4','backend':'software','idempotency_key':'same'}
            api('POST','/processing-jobs',base,401,False)
            first=api('POST','/processing-jobs',base,201);assert api('POST','/processing-jobs',base)['id']==first['id']
            api('POST','/processing-jobs',{**base,'recipe':'audio_aac'},409)
            for job in [first,submit('audio_aac'),submit()]:
                row=terminal(job);assert row['phase']=='completed',row
                choices=api('GET',f"/items/{source['id']}/playback-options")
                rendition=next(v for v in choices['renditions'] if v['file_id']==row['output_file_id']);assert rendition['available']
                data=request(port,'GET',rendition['media_url'])[2];assert data
                output=root/(job['id']+'.mp4');output.write_bytes(data)
                subprocess.run([ffmpeg,'-v','error','-xerror','-i',str(output),'-f','null','-'],check=True,stdout=subprocess.DEVNULL)
            assert len(api('GET','/items')['items'])==1
            assert len(api('GET','/libraries'))==1
            checks.append('three_real_recipes_decode_idempotency_and_original_isolation')
            # Both pipes can flood without a newline. Parsing/storage must stay
            # bounded, and a partial progress line must not prevent cancellation.
            flooded=root/'flood-drained'
            flood_code=('chunk=b"x"*4096\n'
                        'for _ in range(4096): os.write(1,chunk); os.write(2,chunk)\n'
                        'if not pathlib.Path('+repr(str(gate))+').exists(): os.write(1,b"\\nout_time_us=1000000\\n")\n'
                        'pathlib.Path('+repr(str(flooded))+').touch()\n')
            wrapper.write_text(normal_wrapper.replace('while pathlib.Path(',flood_code+'while pathlib.Path(',1))
            try:
                job=submit();row=terminal(job)
                assert flooded.exists() and row['phase']=='completed' and row['output_file_id'],row
                checks.append('stdout_stderr_flood_recovers_into_real_encode')
                flooded.unlink();gate.touch();job=submit()
                wait_for(flooded.exists)
                assert request(port,'GET','/ready')[0]==200
                api('POST',f"/processing-jobs/{job['id']}/control",{'action':'cancel'})
                row=terminal(job)
                assert row['phase']=='cancelled' and row['output_file_id'] is None,row
                checks.append('unterminated_stdout_flood_remains_cancellable')
            finally:
                gate.unlink(missing_ok=True)
                wrapper.write_text(normal_wrapper)
            # Explicit liveness settings distinguish never-started and stalled
            # tools. Repeated valid progress and stderr noise must not renew them.
            stop(process)
            settings=json.loads(config.read_text())
            settings['processing'].update(startup_timeout_seconds=5,no_progress_timeout_seconds=5)
            config.write_text(json.dumps(settings));process=boot()
            try:
                for initial,reason in [('', 'Startup'),('os.write(1,b"out_time_us=1000000\\n")\n', 'NoProgress')]:
                    reported='1000000' if initial else '0'
                    wrapper.write_text('#!'+sys.executable+'\nimport os,time,pathlib\npathlib.Path('+repr(str(pidfile))+').write_text(str(os.getpid()))\n'+initial+'while True:\n os.write(1,b"out_time_us='+reported+'\\n"); os.write(2,b"still alive\\n"); time.sleep(.05)\n')
                    pidfile.unlink(missing_ok=True)
                    server_log=state/'logs/server.log'
                    log_offset=server_log.stat().st_size
                    job=submit();wait_for(pidfile.exists);encoder_pid=int(pidfile.read_text())
                    row=terminal(job)
                    assert row['phase']=='failed' and row['output_file_id'] is None,row
                    wait_for(lambda: ('processing '+reason+' timeout').encode() in server_log.read_bytes()[log_offset:],seconds=5)
                    try: os.kill(encoder_pid,0)
                    except ProcessLookupError: pass
                    else: raise AssertionError('timed out encoder was not reaped')
                    checks.append(reason.lower()+'_deadline_reaps_noisy_encoder_without_publication')
                wrapper.write_text(normal_wrapper)
                assert terminal(submit())['phase']=='completed'
                checks.append('liveness_timeout_releases_capacity_after_reaping')
            finally:
                wrapper.write_text(normal_wrapper);stop(process)
                settings['processing'].pop('startup_timeout_seconds');settings['processing'].pop('no_progress_timeout_seconds')
                config.write_text(json.dumps(settings));process=boot()
            # Advancing media time cannot extend a duration-derived wall-clock budget.
            stop(process)
            settings['processing']['expected_duration']={'allowance_seconds':2,'media_duration_multiplier':1}
            config.write_text(json.dumps(settings));process=boot()
            try:
                wrapper.write_text('#!'+sys.executable+'\nimport os,time,pathlib\npathlib.Path('+repr(str(pidfile))+').write_text(str(os.getpid()))\ni=0\nwhile True:\n i+=100000; os.write(1,("out_time_us="+str(i)+"\\n").encode()); time.sleep(.05)\n')
                pidfile.unlink(missing_ok=True)
                server_log=state/'logs/server.log';log_offset=server_log.stat().st_size
                job=submit();wait_for(pidfile.exists);encoder_pid=int(pidfile.read_text())
                row=terminal(job)
                assert row['phase']=='failed' and row['output_file_id'] is None,row
                assert row['progress_seconds'] > 0,row
                wait_for(lambda: b'processing ExpectedDuration timeout' in server_log.read_bytes()[log_offset:],seconds=5)
                try: os.kill(encoder_pid,0)
                except ProcessLookupError: pass
                else: raise AssertionError('duration-limited encoder was not reaped')
                checks.append('expected_duration_deadline_reaps_advancing_encoder_without_publication')
                wrapper.write_text(normal_wrapper)
                assert terminal(submit())['phase']=='completed'
                checks.append('expected_duration_timeout_releases_capacity_after_reaping')
            finally:
                wrapper.write_text(normal_wrapper);stop(process)
                settings['processing'].pop('expected_duration')
                config.write_text(json.dumps(settings));process=boot()
            hardware={'attempted':sys.platform=='darwin'}
            if sys.platform=='darwin':
                row=terminal(submit(backend='videotoolbox'));hardware.update(phase=row['phase'],error=row['error']);
                if row['phase']=='completed':checks.append('videotoolbox_h264_output_probed_and_decoded_no_software_fallback')
            caps=api('GET','/processing-capabilities');assert caps['worker_concurrency']==1 and 'h264720p' in caps['recipes'] and caps['validated_jobs']
            # Durable event replay begins at a known committed event, including after reconnect.
            with sqlite3.connect(state/'playscale.sqlite3') as db: cursor=db.execute('SELECT max(id) FROM change_events').fetchone()[0]
            gate.touch();job=submit()
            conn=http.client.HTTPConnection('127.0.0.1',port,timeout=5);conn.request('GET','/api/v1/events',headers={'Last-Event-ID':str(cursor)});response=conn.getresponse();assert response.status==200
            event=[]
            while True:
                line=response.readline().decode().strip();event.append(line)
                if not line and any(v.startswith('data:') for v in event):break
            conn.close();assert any('processing' in v for v in event);checks.append('durable_sse_replay')
            wait_for(lambda:api('GET','/processing-jobs/'+job['id'])['phase']=='running')
            api('POST',f"/processing-jobs/{job['id']}/control",{'action':'cancel'})
            assert terminal(job)['phase']=='cancelled';gate.unlink()
            api('POST',f"/processing-jobs/{job['id']}/control",{'action':'retry'})
            assert terminal(job)['phase']=='completed';checks.append('cancel_reap_retry_new_attempt')
            # SIGTERM leaves the active attempt recoverable, while its child is reaped.
            gate.touch();job=submit();wait_for(lambda:api('GET','/processing-jobs/'+job['id'])['phase']=='running');time.sleep(.4)
            old=api('GET','/processing-jobs/'+job['id'])['attempt'];stop(process);gate.unlink();process=boot()
            row=terminal(job);assert row['phase']=='completed' and row['attempt']>old;checks.append('restart_recovery_without_partial_publication')
            # Abrupt server death closes the supervisor pipe and reaps the encoder.
            gate.touch();pidfile.unlink(missing_ok=True);job=submit()
            wait_for(lambda:pidfile.exists());encoder_pid=int(pidfile.read_text())
            process.kill();process.wait(timeout=5)
            def dead():
                try:os.kill(encoder_pid,0);return False
                except ProcessLookupError:return True
            wait_for(dead,seconds=5);gate.unlink();process=boot()
            assert terminal(job)['phase']=='completed';checks.append('sigkill_reaps_encoder_and_recovers_attempt')
            # Replacement without a scan must fail the final physical source check.
            gate.touch();job=submit();wait_for(lambda:api('GET','/processing-jobs/'+job['id'])['phase']=='running');time.sleep(.5)
            original_bytes=(media/'clip.mp4').read_bytes();(media/'clip.mp4').write_bytes(original_bytes+b'changed');gate.unlink()
            row=terminal(job);assert row['phase']=='failed' and row['output_file_id'] is None;checks.append('source_replacement_rejects_result')
            (media/'clip.mp4').write_bytes(original_bytes)
            scan=api('POST',f'/libraries/{library}/scans',None,202)
            wait_for(lambda:api('GET','/jobs/'+scan['id'])['phase']=='completed')
            source=scanned()
            # An explicit cache budget rejects work before allocating its source snapshot.
            filler=state/'generated'/'budget-fixture';filler.write_bytes(b'x'*(30*1024*1024));job=submit()
            row=terminal(job);assert row['phase']=='failed' and row['output_file_id'] is None;filler.unlink();checks.append('cache_budget_rejects_work_before_snapshot')
            # Encoder startup failure produces no output and exposes a terminal result.
            wrapper.rename(root/'disabled-encoder');job=submit();row=terminal(job);assert row['phase']=='failed' and row['output_file_id'] is None;wrapper=(root/'disabled-encoder');wrapper.rename(root/'ffmpeg-wrapper');checks.append('encoder_failure_is_terminal_without_rendition')
            # Exit zero is not sufficient: corrupt output must not be published.
            wrapper=root/'ffmpeg-wrapper'
            wrapper.write_text('#!'+sys.executable+'\nimport pathlib,sys\npathlib.Path(sys.argv[-1]).write_bytes(b"not media")\n');wrapper.chmod(0o700)
            job=submit();row=terminal(job);assert row['phase']=='failed' and row['output_file_id'] is None
            wrapper.write_text(normal_wrapper);wrapper.chmod(0o700);checks.append('successful_exit_with_corrupt_output_is_rejected')
            api('PUT',f'/admin/scan-schedules/{library}',{'interval_seconds':60})
            with sqlite3.connect(state/'playscale.sqlite3') as db:
                before=db.execute('SELECT count(*) FROM jobs').fetchone()[0];db.execute('UPDATE scan_schedules SET next_run=0')
            wait_for(lambda:len(api('GET','/admin/scan-schedules'))==1)
            deadline=time.monotonic()+35
            while time.monotonic()<deadline:
                with sqlite3.connect(state/'playscale.sqlite3') as db: count=db.execute('SELECT count(*) FROM jobs').fetchone()[0]
                if count>before:break
                time.sleep(.2)
            assert count>before;api('PUT',f'/admin/scan-schedules/{library}',{'interval_seconds':0});checks.append('scheduled_scan_and_disable')
            backup(state,root/'snapshot');restore(root/'snapshot',root/'restored');stop(process);process=boot(root/'restored')
            old_auth=auth;auth={'Authorization':'Bearer '+(root/'restored/admin-token').read_text().strip()}
            choices=api('GET',f"/items/{source['id']}/playback-options");assert all(not r['available'] for r in choices['renditions'])
            assert list((state/'generated').glob('*/*/output.mp4'))
            stop(process);process=boot();auth=old_auth;checks.append('db_only_restore_invalidates_foreign_generated_cache')
            with sqlite3.connect(state/'playscale.sqlite3') as db:db.execute("UPDATE processing_jobs SET updated_at=0 WHERE phase='completed'")
            cache=api('POST','/admin/cache');assert cache['bytes']==0,cache
            assert all(not r['available'] for r in api('GET',f"/items/{source['id']}/playback-options")['renditions'])
            assert hashlib.sha256((media/'clip.mp4').read_bytes()).hexdigest()==original;checks.append('cache_expiration_preserves_original_bytes')
            spec=api('GET','/openapi.json');assert '/api/v1/events' in spec['paths'] and '/api/v1/processing-jobs' in spec['paths']
            print(json.dumps({'passed':len(checks),'checks':checks,'hardware':hardware},indent=2))
        except BaseException:
            log.flush();print((root/'log').read_text()[-8000:],file=sys.stderr)
            server_log=state/'logs/server.log'
            if server_log.exists(): print(server_log.read_text()[-8000:],file=sys.stderr)
            raise
        finally:
            if process is not None and process.poll() is None:stop(process)
            log.close()
if __name__=='__main__':main()

"""Real HTTP + FFmpeg qualification of standalone rendition profiles; no browser claims."""
import argparse, hashlib, http.client, json, pathlib, shutil, subprocess, tempfile, time
from smoke import free_port, request, wait_for, stop


def main():
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=pathlib.Path,required=True);parser.add_argument('--receipt',type=pathlib.Path);args=parser.parse_args()
    results=[]
    with tempfile.TemporaryDirectory(prefix='playscale-profiles-') as directory:
        root=pathlib.Path(directory);media=root/'media';media.mkdir();state=root/'state';port=free_port()
        source_path=media/'source.mp4';ffmpeg=shutil.which('ffmpeg');assert ffmpeg
        subprocess.run([ffmpeg,'-v','error','-f','lavfi','-i','testsrc2=size=640x360:rate=60','-f','lavfi','-i','sine=frequency=440:sample_rate=48000','-t','4.7','-c:v','libx264','-threads','2','-pix_fmt','yuv420p','-c:a','aac',str(source_path)],check=True)
        source_sha=hashlib.sha256(source_path.read_bytes()).hexdigest()
        config=root/'config.json';config.write_text(json.dumps({'access_mode':'trusted_household','listen':f'127.0.0.1:{port}','data_dir':str(state),'libraries':[str(media)],'processing':{'ffmpeg':ffmpeg,'cache_bytes':128*1024*1024,'max_output_bytes':16*1024*1024,'timeout_seconds':60}}))
        log=(root/'server.log').open('wb');process=subprocess.Popen([str(args.binary.resolve()),'--config',str(config)],stdout=log,stderr=log)
        try:
            wait_for(lambda:request(port,'GET','/ready')[0]==200);auth={'Authorization':'Bearer '+(state/'admin-token').read_text().strip()}
            def api(method,path,body=None,status=200):
                code,_,data=request(port,method,'/api/v1'+path,body,auth);assert code==status,(path,code,data.decode());return json.loads(data)
            source=wait_for(lambda:next(iter(api('GET','/items')['items']),None));assert source['tracks'][0]['width']==640
            # Record initial snapshot cursor; later reconnect must replay committed changes.
            connection=http.client.HTTPConnection('127.0.0.1',port,timeout=10);connection.request('GET','/api/v1/events');response=connection.getresponse();cursor=None
            while True:
                line=response.readline().decode().strip()
                if line.startswith('id:'):cursor=int(line[3:].strip())
                if not line and cursor is not None:break
            connection.close()
            def terminal(job):
                row=api('GET','/processing-jobs/'+job['id']);return row if row['phase'] in ['completed','failed','cancelled'] else None
            jobs=[]
            profiles=[('h264',320,180,{'numerator':30,'denominator':1}),('hevc',320,180,None),('h264',640,360,{'numerator':30000,'denominator':1001}),('h264',320,180,{'numerator':1,'denominator':1}),('h264',320,180,{'numerator':2,'denominator':1})]
            for codec,width,height,fps in profiles:
                profile={'version':1,'codec':codec,'max_width':width,'max_height':height,'video_bitrate':500000,'audio_bitrate':96000,'frame_rate':fps}
                body={'source_file_id':source['file_id'],'source_revision':source['revision'],'recipe':'video_profile','backend':'software','video_profile':profile,'idempotency_key':str(time.time_ns())}
                job=api('POST','/processing-jobs',body,201);assert job['output_file_id'] is None;assert api('POST','/processing-jobs',body)['id']==job['id'];jobs.append(job['id'])
                row=wait_for(lambda:terminal(job),seconds=65);assert row['phase']=='completed',row
                choices=api('GET',f'/items/{source["id"]}/playback-options');variant=next(v for v in choices['renditions'] if v['file_id']==row['output_file_id'])
                track=next(t for t in variant['tracks'] if t['kind']=='video');assert track['codec']==codec and track['width']==width and track['height']==height,track
                actual_fps=track['average_frame_rate']['numerator']/track['average_frame_rate']['denominator'];wanted=60 if fps is None else fps['numerator']/fps['denominator'];assert abs(actual_fps-wanted)<.01,(actual_fps,wanted)
                assert abs(variant['duration_seconds']-source['duration_seconds'])<=.5,(variant['duration_seconds'],source['duration_seconds'])
                assert variant['average_bitrate']>0 and variant['available'];assert variant['recipe']['video_profile']==profile
                plan=api('POST',f'/profiles/default/items/{source["id"]}/playback-plan',{'mode':'convert','recipe':'video_profile','video_profile':profile});assert plan['status']=='ready' and plan['selection']['file_id']==row['output_file_id'],plan
                code,headers,payload=request(port,'GET',variant['media_url']);assert code==200;assert hashlib.sha256(payload).hexdigest()==variant['file_revision']
                assert request(port,'GET',variant['media_url'],headers={'Range':'bytes=0-31'})[2]==payload[:32]
                copy=root/(job['id']+'.mp4');copy.write_bytes(payload);subprocess.run([ffmpeg,'-v','error','-xerror','-i',str(copy),'-f','null','-'],check=True)
                results.append({'profile':profile,'job_id':job['id'],'file_revision':variant['file_revision'],'tracks':variant['tracks'],'bytes':variant['bytes'],'duration_seconds':variant['duration_seconds'],'average_bitrate':variant['average_bitrate']})
            connection=http.client.HTTPConnection('127.0.0.1',port,timeout=10);connection.request('GET','/api/v1/events',headers={'Last-Event-ID':str(cursor)});response=connection.getresponse();seen=set();data={};last_id=cursor
            while not set(jobs).issubset(seen):
                line=response.readline().decode().strip()
                if line.startswith('id:'):assert int(line[3:].strip())>last_id;last_id=int(line[3:].strip())
                if line.startswith('data:'):
                    data=json.loads(line[5:]);
                    if data.get('topic')=='processing':seen.add(data['resource_id'])
            connection.close()
            # Same files survive a server restart with their immutable profile metadata.
            stop(process);process=subprocess.Popen([str(args.binary.resolve()),'--config',str(config)],stdout=log,stderr=log);wait_for(lambda:request(port,'GET','/ready')[0]==200)
            assert len(api('GET',f'/items/{source["id"]}/playback-options')['renditions'])==len(profiles)
            assert hashlib.sha256(source_path.read_bytes()).hexdigest()==source_sha
        except BaseException:
            log.flush();print((root/'server.log').read_text());raise
        finally:stop(process);log.close()
    receipt={'source_sha256':source_sha,'binary_sha256':hashlib.sha256(args.binary.read_bytes()).hexdigest(),'encodes':results,'sse_replay':True,'restart_preserves_profiles':True,'range_delivery':True,'fully_decoded':True,'browser_switching_qualified':False,'hardware_qualified':False}
    if args.receipt:args.receipt.write_text(json.dumps(receipt,indent=2)+'\n')
    print(json.dumps(receipt,indent=2))
if __name__=='__main__':main()

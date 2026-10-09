"""Real-process HTTP acceptance test. Generated media, disposable databases, no user library."""
import hashlib,http.client,json,os,pathlib,signal,socket,subprocess,tempfile,time
ROOT=pathlib.Path(__file__).resolve().parents[1]
BINARY=ROOT/'target/debug/playscale'

def free_port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]

def request(port,method,path,body=None,headers=None):
    headers=dict(headers or {})
    if body is not None:body=json.dumps(body);headers['Content-Type']='application/json'
    conn=http.client.HTTPConnection('127.0.0.1',port,timeout=10)
    try:
        conn.request(method,path,body,headers);r=conn.getresponse();return r.status,dict(r.getheaders()),r.read()
    finally:conn.close()

def wait_for(fn,seconds=20):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        try:
            value=fn()
            if value:return value
        except (OSError,http.client.HTTPException):pass
        time.sleep(.05)
    raise AssertionError('Timed out')

def stop(process):
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
        try:process.wait(timeout=15)
        except subprocess.TimeoutExpired:process.kill();process.wait();raise AssertionError('Server failed bounded shutdown')
    assert process.returncode==0,process.returncode

def main():
    checks=[]
    with tempfile.TemporaryDirectory(prefix='playscale-smoke-') as directory:
        base=pathlib.Path(directory);media=base/'media';media.mkdir();state=base/'state';fixture=media/'Signal-Garden.mp4'
        subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-f','lavfi','-i','testsrc2=size=320x180:rate=24','-f','lavfi','-i','sine=frequency=440:sample_rate=48000','-t','3','-c:v','libx264','-pix_fmt','yuv420p','-c:a','aac','-movflags','+faststart',str(fixture)],check=True)
        payload=fixture.read_bytes();port=free_port();log=(base/'server.log').open('wb')
        command=[str(BINARY),'--access-mode','trusted_household','--listen',f'127.0.0.1:{port}','--data-dir',str(state),'--library',str(media),'--demuxe-dir',str(ROOT/'web/vendor/demuxe')]
        process=subprocess.Popen(command,stdout=log,stderr=subprocess.STDOUT)
        try:
            wait_for(lambda:request(port,'GET','/health')[0]==200)
            def catalog():
                status,_,body=request(port,'GET','/api/v1/items');assert status==200
                items=json.loads(body)['items'];return items[0] if items else None
            item=wait_for(catalog);assert abs(item['duration_seconds']-3)<.1;assert len(item['tracks'])==2;checks.append('real_ffprobe_catalog')
            status,headers,body=request(port,'GET',item['media_url']);assert status==200 and body==payload;checks.append('original_bytes_sha256')
            for method,extra,status_expected,expected in [('GET',{'Range':'bytes=2-99999999'},206,payload[2:]),('GET',{'Range':'bytes=-12'},206,payload[-12:]),('HEAD',{'Range':'bytes=0-9'},200,b''),('GET',{'Range':'bytes=0-9','If-Range':'"stale"'},200,payload),('GET',{'If-None-Match':headers['etag']},304,b'')]:
                status,_,body=request(port,method,item['media_url'],headers=extra);assert status==status_expected and body==expected,(method,extra,status)
            checks.append('wire_ranges_and_validators')
            token=(state/'admin-token').read_text().strip();auth={'Authorization':'Bearer '+token}
            assert request(port,'POST','/api/v1/libraries',{'name':'unauthorized','root':str(media)})[0]==401
            assert request(port,'PUT',f'/api/v1/items/{item["id"]}/metadata/catabolic',{'expected_revision':0,'external_id':'test-item','values':{'title':'Imported Signal'},'tags':['Test']},dict(auth,Origin='https://untrusted.invalid'))[0]==403
            status,_,body=request(port,'PUT',f'/api/v1/items/{item["id"]}/metadata/catabolic',{'expected_revision':0,'external_id':'test-item','values':{'title':'Imported Signal'},'tags':['Test']},auth);assert status==200 and json.loads(body)['tags']==['test'];checks.append('authenticated_metadata_import')
            progress=f'/api/v1/profiles/default/progress/{item["id"]}'
            assert request(port,'PUT',progress,{'position_seconds':1.25})[0]==200
            status,_,body=request(port,'GET','/api/v1/openapi.json');assert status==200 and '/api/v1/items/{id}/metadata/{source}' in json.loads(body)['paths'];checks.append('public_openapi')
            stop(process)
            process=subprocess.Popen(command,stdout=log,stderr=subprocess.STDOUT);wait_for(lambda:request(port,'GET','/health')[0]==200)
            assert json.loads(request(port,'GET',progress)[2])['position_seconds']==1.25;assert catalog()['title']=='Imported Signal';checks.append('restart_preserves_progress_and_curation')
            # A second owner must fail without touching the running catalog.
            second=subprocess.run([str(BINARY),'--access-mode','trusted_household','--listen',f'127.0.0.1:{free_port()}','--data-dir',str(state)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
            assert second.returncode!=0;checks.append('single_owner_lock')
        finally:
            stop(process);log.close()
    print(json.dumps({'checks':checks,'passed':len(checks),'fixture_sha256':hashlib.sha256(payload).hexdigest()},indent=2))

if __name__=='__main__':main()

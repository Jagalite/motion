#!/usr/bin/env python3
"""Check provenance integrity and prevent known negative receipts becoming passes."""
import hashlib,json,pathlib,subprocess,sys
root=pathlib.Path(__file__).resolve().parent
for name,digest in json.loads((root/'evidence-sha256.json').read_text()).items():
 assert hashlib.sha256((root/name).read_bytes()).hexdigest()==digest,name
for name,code,verdict in [('before.json.gz',1,'FAIL'),('after.json.gz',1,'FAIL'),('inactive-preview.json',2,'INCONCLUSIVE')]:
 run=subprocess.run([sys.executable,str(root/'analyze.py'),str(root/name)],capture_output=True,text=True)
 assert run.returncode==code,(name,run.stderr)
 result=json.loads(run.stdout);assert result['video_decoded_continuity']==verdict,name
 assert result['frame_perfect_gapless_qualified'] is False,name
 assert result['audio_output']=='NOT_MEASURED',name
 if code==1:assert result['coverage_sufficient'],name
for result in json.loads((root/'fixture-verification.json').read_text()):
 assert result['valid'] and result['frames']==3600 and result['wrong_count']==0
for name in ['before','after']:
 assert json.loads((root/(name+'-provenance.json')).read_text())['served_matches_snapshot']
print('Evidence integrity and negative/incomplete qualification guards: PASS')
print('Product qualification remains NOT QUALIFIED; audio output was not measured.')

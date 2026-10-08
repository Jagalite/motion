#!/usr/bin/env python3
"""Verify every encoded counter and frame timestamp in source and rendition."""
import argparse,hashlib,json,pathlib,subprocess
import numpy as np
p=argparse.ArgumentParser();p.add_argument('files',type=pathlib.Path,nargs='+');args=p.parse_args();results=[]
for path in args.files:
 raw=subprocess.check_output(['ffmpeg','-v','error','-i',str(path),'-an','-vf','crop=320:2:0:30','-f','rawvideo','-pix_fmt','rgb24','pipe:1'])
 frames=np.frombuffer(raw,dtype=np.uint8).reshape(-1,2,320,3);bits=frames[:,0,np.arange(12)*24+12,0]>120;ids=(bits*(1<<np.arange(12))).sum(axis=1)
 metadata=json.loads(subprocess.check_output(['ffprobe','-v','error','-select_streams','v:0','-show_frames','-show_entries','frame=best_effort_timestamp_time','-of','json',str(path)],text=True))
 pts=np.array([float(f['best_effort_timestamp_time']) for f in metadata['frames']]);wrong=np.flatnonzero(ids!=np.arange(len(ids)));error=float(np.max(np.abs(pts-np.arange(len(pts))/30)))
 results.append({'file':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'frames':len(ids),'wrong_count':len(wrong),'wrong_ids':wrong[:20].tolist(),'max_pts_error_seconds':error,'valid':not len(wrong) and len(pts)==len(ids) and error<.000001})
print(json.dumps(results,indent=2))
raise SystemExit(0 if all(r['valid'] for r in results) else 1)

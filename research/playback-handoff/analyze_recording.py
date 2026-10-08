#!/usr/bin/env python3
"""Decode visible frame IDs from this run's 2804px-wide tab recording.

Crop is bound to the measured video rect: CSS x=150, y=199.625,
width=1102, height=619.875, capture scale=2. This is not a general UI locator.
The recorder may drop frames; absence of a reversal cannot establish a pass.
"""
import argparse,json,pathlib,subprocess
import numpy as np
p=argparse.ArgumentParser();p.add_argument('recording',type=pathlib.Path);args=p.parse_args()
raw=subprocess.check_output(['ffmpeg','-v','error','-i',str(args.recording),'-an','-vf','crop=2204:2:300:606,scale=320:2:flags=neighbor','-fps_mode','passthrough','-enc_time_base','1:1000000','-pix_fmt','rgb24','-f','rawvideo','pipe:1'])
frames=np.frombuffer(raw,dtype=np.uint8).reshape(-1,2,320,3);values=frames[:,0,np.arange(12)*24+12,0];ids=((values>120)*(1<<np.arange(12))).sum(axis=1);valid=np.all((values<80)|(values>176),axis=1)&(frames[:,0,300,0]>40)&(frames[:,0,300,0]<88)
meta=json.loads(subprocess.check_output(['ffprobe','-v','error','-select_streams','v:0','-show_frames','-show_entries','frame=best_effort_timestamp_time','-of','json',str(args.recording)],text=True));pts=[float(f['best_effort_timestamp_time']) for f in meta['frames']]
assert len(pts)==len(ids)
samples=[{'time':t,'frame':int(i),'valid':bool(v)} for t,i,v in zip(pts,ids,valid)]
backwards=[];prior=None
for row in samples:
 if row['valid']:
  if prior and row['frame']<prior['frame']:backwards.append({'time':row['time'],'from':prior['frame'],'to':row['frame'],'delta':row['frame']-prior['frame']})
  prior=row
print(json.dumps({'recording':str(args.recording),'samples':samples,'backwards':backwards,'audio_measured':False,'positive_qualification_possible':False},indent=2))

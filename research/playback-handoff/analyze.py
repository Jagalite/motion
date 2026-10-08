#!/usr/bin/env python3
"""Report decoded-pixel continuity, keeping baseline scheduling limitations visible."""
import argparse,gzip,json,pathlib
p=argparse.ArgumentParser();p.add_argument('receipt',type=pathlib.Path);args=p.parse_args();data=json.loads(gzip.decompress(args.receipt.read_bytes()) if args.receipt.suffix=='.gz' else args.receipt.read_bytes());samples=data['samples'];result={'receipt':str(args.receipt),'samples':len(samples),'errors':data['errors'],'transitions':[],'windows':[]}
for i,row in enumerate(samples[1:],1):
 if row['element']!=samples[i-1]['element']:
  old=samples[i-1];result['transitions'].append({'wall':row['wall'],'from_element':old['element'],'to_element':row['element'],'old_frame':old['frame'],'new_frame':row['frame'],'frame_delta':None if old['frame'] is None or row['frame'] is None else row['frame']-old['frame'],'native_clock_delta':row['mediaTime']-old['mediaTime'],'sample_interval_ms':row['wall']-old['wall'],'nearby':samples[max(0,i-4):i+5]})
marks=[m for m in data['marks'] if m['kind']=='test']
for n,mark in enumerate(marks):
 end=marks[n+1]['wall'] if n+1<len(marks) else float('inf');window=[s for s in samples if mark['wall']<=s['wall']<end]
 jumps=[];invalid=sum(not s['valid'] for s in window);previous=None;run_start=None;longest=0;maximum_interval=0
 for a,b in zip(window,window[1:]):maximum_interval=max(maximum_interval,b['wall']-a['wall'])
 for s in window:
  if not s['valid']:continue
  if previous is not None and s['frame']!=previous['frame']:
   delta=s['frame']-previous['frame']
   if delta!=1:jumps.append({'wall':s['wall'],'from':previous['frame'],'to':s['frame'],'delta':delta,'elements':[previous['element'],s['element']]})
   if run_start is not None:longest=max(longest,s['wall']-run_start)
   run_start=s['wall']
  elif previous is None:run_start=s['wall']
  previous=s
 result['windows'].append({'label':mark['label'],'samples':len(window),'invalid_samples':invalid,'nonconsecutive_frames':jumps,'longest_frame_hold_ms':longest,'max_sampling_interval_ms':maximum_interval})
baseline=next((w for w in result['windows'] if w['label']=='baseline-steady'),None)
trials=[w for w in result['windows'] if w['label'].startswith('switch-')]
coverage=bool(baseline and baseline['samples']>=120 and len(trials)==4 and len(result['transitions'])==4 and all(w['samples']>=120 and w['max_sampling_interval_ms']<33.334 for w in [baseline,*trials]))
failures=[w['label'] for w in ([baseline] if baseline else [])+trials if w['nonconsecutive_frames'] or w['invalid_samples'] or w['longest_frame_hold_ms']>50]
result['coverage_sufficient']=coverage
result['video_decoded_continuity']='FAIL' if failures else 'PASS_FOR_OBSERVED_TRIALS' if coverage else 'INCONCLUSIVE'
result['failed_windows']=failures
result['audio_output']='NOT_MEASURED'
result['frame_perfect_gapless_qualified']=False
print(json.dumps(result,indent=2))
raise SystemExit(1 if failures else 0 if coverage else 2)

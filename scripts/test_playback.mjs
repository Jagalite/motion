import assert from 'node:assert/strict';
import {attemptPlayback, preparationSubmitter, sourceForPlayback} from '../web/playback.js';
const selection=(file_id,revision='v1')=>({file_id,revision,media_url:`/media/${file_id}?revision=${revision}`});
let opened=[];
const plan=await attemptPlayback({
  getPlan:async failed=>({status:'ready',selection:selection(failed.length?'rendition':'original')}),
  open:async v=>{opened.push(v.file_id);if(v.file_id==='original')throw new Error('open failed');}
});
assert.deepEqual(opened,['original','rendition']);assert.equal(plan.selection.file_id,'rendition');
let calls=0;
const blocked=await attemptPlayback({getPlan:async failed=>failed.length?{status:'blocked'}:{status:'ready',selection:selection('original')},open:async()=>{calls++;throw new Error('failed');}});
assert.equal(blocked.status,'blocked');assert.equal(calls,1);
const prepare=await attemptPlayback({getPlan:async()=>({status:'preparation_required'}),open:async()=>{throw new Error('must not open');}});
assert.equal(prepare.status,'preparation_required');
await assert.rejects(attemptPlayback({getPlan:async()=>({status:'ready',selection:selection('same')}),open:async()=>{throw new Error('failed');}}),/repeated a failed version/);
await assert.rejects(attemptPlayback({maxAttempts:2,getPlan:async failed=>({status:'ready',selection:selection(String(failed.length))}),open:async()=>{throw new Error('failed');}}),/attempts exhausted/);
console.log('Planner client: bounded fallback, strict blocking, preparation boundary, repeated-plan protection PASS');

// The final failed open must still produce a blocked/preparation plan, without
// exceeding the open budget or hiding the user's next action.
let boundedOpens=0;
const finalPlan=await attemptPlayback({maxAttempts:2,
  getPlan:async failed=>failed.length===2?{status:'preparation_required'}:{status:'ready',selection:selection(String(failed.length))},
  open:async()=>{boundedOpens++;throw new Error('failed');}
});
assert.equal(finalPlan.status,'preparation_required');assert.equal(boundedOpens,2);
// Lost HTTP replies reuse the exact key; a later deliberate attempt gets a new one.
let nextKey=0, loseReply=true;const requests=[];
const submit=preparationSubmitter(async body=>{requests.push(body);if(loseReply){loseReply=false;throw new Error('lost reply');}return {id:'job',phase:'failed'};},()=>String(++nextKey));
const proposal={source_file_id:'source',source_revision:'r1',recipe:'h264720p',backend:'software'};
await assert.rejects(submit(proposal),/lost reply/);
await submit({...proposal,job_id:null});await submit(proposal);
assert.deepEqual(requests.map(r=>r.idempotency_key),['1','1','2']);
assert.deepEqual(requests[0],requests[1]);
// Switching mode/recipe keeps the original cut; choosing another explicit version
// intentionally changes the source, including when that version is a rendition.
const item={file_id:'default-cut',revision:'d1'}, chosen={file_id:'directors-cut',revision:'c1'};
assert.deepEqual(sourceForPlayback(item,null,chosen),chosen);
assert.deepEqual(sourceForPlayback(item,{source_file_id:'other-cut',source_revision:'o1'},chosen),{file_id:'other-cut',revision:'o1'});
assert.deepEqual(sourceForPlayback(item,{file_id:'new-original',revision:'n1'},chosen),{file_id:'new-original',revision:'n1'});
console.log('Review regressions: final failure plan, idempotency lifecycle, source preservation PASS');
const {PlaybackSwitches}=await import('../web/playback.js');
const owner=new PlaybackSwitches(),first=owner.begin();
const waiting=first.delay(10000);const cancelled=assert.rejects(waiting,{name:'AbortError'});
const second=owner.begin();await cancelled;
assert.equal(first.current,false);assert.throws(()=>first.check(),{name:'AbortError'});
first.finish();assert.equal(owner.pending,true);await second.delay(1);
owner.cancel();assert.equal(second.current,false);assert.equal(owner.pending,false);
console.log('Latest switch wins, stale completion protection, cancellable preparation: PASS');
{
const {transferPlayback}=await import('../web/playback.js');
const makeCore=state=>({state:{...state},calls:[],async seek(value){this.calls.push(['seek',value]);this.state.currentTime=value;},async setPlaybackRate(value){this.state.playbackRate=value;},async setVolume(value){this.state.volume=value;},async setMuted(value){this.calls.push(['mute',value]);this.state.muted=value;},async play(){this.state.status='playing';},async pause(){this.state.status='paused';}});
const from=makeCore({currentTime:10,status:'playing',playbackIntent:'play',volume:.3,muted:false,playbackRate:1.5}),to=makeCore({muted:true});
const selection={file_id:'converted',revision:'r'},previous={file_id:'original',revision:'r'};
let releaseAck,ackStarted,finalSaved,acknowledged=false;
const ack=new Promise(r=>ackStarted=r);
const transfer=transferPlayback({from,to,selection,previous,session:{async update(time,status,file){if(!acknowledged){acknowledged=true;ackStarted();await new Promise(r=>releaseAck=r);}else{finalSaved={time,status};}}}});
await ack;assert.equal(from.state.muted,false);assert.equal(from.state.status,'playing');
// A user pause/seek while the server acknowledges must override the initial sample.
Object.assign(from.state,{currentTime:42,status:'paused',playbackIntent:'pause'});releaseAck();await transfer;
assert.equal(to.state.currentTime,42);assert.equal(to.state.status,'paused');assert.equal(to.state.volume,.3);assert.equal(to.state.playbackRate,1.5);assert.equal(to.state.muted,false);assert.equal(from.state.muted,true);assert.deepEqual(finalSaved,{time:42,status:'paused'});
const failedFrom=makeCore({currentTime:50,status:'playing',playbackIntent:'play',volume:.8,muted:false,playbackRate:1});
const failedTo=makeCore({muted:true});failedTo.setMuted=async value=>{if(!value)throw new Error('activation failed');};
const events=[];
await assert.rejects(transferPlayback({from:failedFrom,to:failedTo,selection,previous,session:{async update(time,status,file){events.push(file);}}}),/activation failed/);
assert.deepEqual(events,[selection,selection,previous]);assert.equal(failedFrom.state.muted,false);assert.equal(failedFrom.state.status,'playing');
const rejectedFrom=makeCore({...failedFrom.state}),rejectedTo=makeCore({muted:true});
await assert.rejects(transferPlayback({from:rejectedFrom,to:rejectedTo,selection,previous,session:{async update(){throw new Error('revision conflict');}}}),/revision conflict/);
assert.equal(rejectedFrom.state.muted,false);assert.equal(rejectedTo.state.muted,true);
console.log('Live transport sampling, uninterrupted original, activation rollback and rejected handoff: PASS');

// A seek during the replacement's final seek must be re-applied and saved.
const lateFrom=makeCore({currentTime:10,status:'playing',playbackIntent:'play',volume:1,muted:false,playbackRate:1});
let notify=()=>{},stops=0;
lateFrom.subscribe=callback=>{notify=callback;callback(lateFrom.state);return ()=>{stops++;};};
const lateTo=makeCore({muted:true});let seekCount=0;
lateTo.seek=async value=>{
  lateTo.state.currentTime=value;
  if(++seekCount===2){Object.assign(lateFrom.state,{currentTime:70,status:'paused',playbackIntent:'pause',pendingOperation:{id:9,kind:'seeking'}});notify(lateFrom.state);}
};
const lateEvents=[];
await transferPlayback({from:lateFrom,to:lateTo,selection,previous,session:{async update(time,status,file){lateEvents.push({time,status,file});}}});
assert.equal(lateTo.state.currentTime,70);assert.equal(lateTo.state.status,'paused');
assert.deepEqual(lateEvents.at(-1),{time:70,status:'paused',file:selection});assert.ok(stops>0);
// Even failure to restore audio cannot prevent the server rollback attempt.
const brokenFrom=makeCore({...failedFrom.state});brokenFrom.setMuted=async value=>{if(!value)throw new Error('restore failed');};
const brokenTo=makeCore({muted:true});brokenTo.setMuted=async value=>{if(!value)throw new Error('activation failed');};
const rollback=[];
await assert.rejects(transferPlayback({from:brokenFrom,to:brokenTo,selection,previous,session:{async update(time,status,file){rollback.push(file);}}}),AggregateError);
assert.equal(rollback.at(-1),previous);
console.log('Late seek persisted, late pause retained, rollback survives cleanup errors: PASS');

}

const cancellationOwner=new PlaybackSwitches(),resourceTask=cancellationOwner.begin();let releases=0;
resourceTask.onCancel(()=>releases++);
cancellationOwner.begin();assert.equal(releases,1);assert.equal(resourceTask.signal.aborted,true);
resourceTask.onCancel(()=>releases++);assert.equal(releases,2);
const detached=cancellationOwner.begin();const release=detached.onCancel(()=>releases++);release();cancellationOwner.cancel();assert.equal(releases,2);
console.log('Superseded resource cleanup, aborted requests, detached active resource: PASS');
{
const {playbackSnapshot,transferPlayback}=await import('../web/playback.js');
const from={state:{currentTime:10,status:'playing',playbackIntent:'play',volume:1,muted:false,playbackRate:1},diagnostics:{backend:{path:'native',position:10.24}},async setMuted(){}};
const seeks=[],events=[];
const to={async seek(time){seeks.push(time);},async setPlaybackRate(){},async setVolume(){},async play(){},async setMuted(){}};
await transferPlayback({from,to,selection:selection('converted'),previous:selection('original'),session:{async update(time,status){events.push({time,status});}}});
assert.ok(seeks.every(time=>time===10.24));assert.ok(events.every(event=>event.time===10.24));
assert.equal(from.state.currentTime,10); // Never mutate the published snapshot.
from.diagnostics.backend={path:'native',position:NaN};assert.equal(playbackSnapshot(from).currentTime,10);
from.diagnostics.backend={path:'software',position:20};assert.equal(playbackSnapshot(from).currentTime,10);
console.log('Handoff uses fresh native source clock, retains safe fallback for other routes: PASS');
}
{
const {transferPlayback}=await import('../web/playback.js');
// Change tracks during each await boundary, including the final server ack.
for(const stage of ['initial-ack','track-selection','final-seek','final-ack']){
  const tracks=prefix=>[{id:prefix+'1',language:'en',label:'English',selected:true},{id:prefix+'2',language:'fr',label:'French',selected:false}];
  const state={currentTime:10,status:'playing',playbackIntent:'play',volume:1,muted:false,playbackRate:1,audioTracks:tracks('a'),subtitleTracks:tracks('s'),subtitlesVisible:true};
  const from={state,async setMuted(value){this.state.muted=value;}};
  let changed=false;
  const change=()=>{if(changed)return;changed=true;for(const key of ['audioTracks','subtitleTracks'])for(const track of state[key])track.selected=track.language==='fr';state.subtitlesVisible=false;};
  let seeks=0,acks=0;
  const to={state:{audioTracks:tracks('other-a'),subtitleTracks:tracks('other-s'),subtitlesVisible:true},async seek(){if(++seeks===2&&stage==='final-seek')change();},async setPlaybackRate(){},async setVolume(){},async play(){},async setMuted(){},async selectAudioTrack(id){for(const t of this.state.audioTracks)t.selected=t.id===id;if(stage==='track-selection')change();},async selectSubtitleTrack(id){for(const t of this.state.subtitleTracks)t.selected=t.id===id;},async subtitleVisible(value){this.state.subtitlesVisible=value;}};
  if(stage==='track-selection')to.state.audioTracks.forEach(t=>t.selected=false);
  let activated=false;
  await transferPlayback({from,to,selection:selection('converted'),previous:selection('original'),session:{async update(){acks++;if((stage==='initial-ack'&&acks===1)||(stage==='final-ack'&&acks===2))change();}},activate(){activated=true;assert.equal(to.state.audioTracks.find(t=>t.selected).language,'fr');assert.equal(to.state.subtitleTracks.find(t=>t.selected).language,'fr');assert.equal(to.state.subtitlesVisible,false);}});
  assert.equal(changed,true);assert.equal(activated,true);
}
console.log('Track and subtitle changes during selection, seek, initial/final acknowledgement retained: PASS');
}

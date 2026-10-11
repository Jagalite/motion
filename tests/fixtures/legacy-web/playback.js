// An open failure excludes only that exact version for this attempt. It does not
// assert a codec incompatibility and never submits a processing job.
export async function attemptPlayback({getPlan, open, onFailure = () => {}, maxAttempts = 8}) {
  const failed = [];
  for (let attempt = 0; ; attempt++) {
    const plan = await getPlan(failed);
    if (plan.status !== 'ready') return plan;
    if (attempt >= maxAttempts) throw new Error('Playback attempts exhausted. Choose another mode or version.');
    const identity = {file_id: plan.selection.file_id, revision: plan.selection.revision};
    if (failed.some(v => v.file_id === identity.file_id && v.revision === identity.revision)) {
      throw new Error('Planner repeated a failed version');
    }
    try {
      await open(plan.selection);
      return plan;
    } catch (error) {
      failed.push(identity);
      await onFailure(error);
    }
  }
}

// Preserve the key only until an outcome is received. A deliberate new attempt
// after a failed/cancelled/expired job must not replay that terminal job forever.
export function preparationSubmitter(submit, newKey = () => crypto.randomUUID()) {
  const pending = new Map();
  return async proposal => {
    const body = {
      source_file_id: proposal.source_file_id,
      source_revision: proposal.source_revision,
      recipe: proposal.recipe,
      backend: proposal.backend,
    };
    const signature = JSON.stringify(body);
    if (!pending.has(signature)) pending.set(signature, newKey());
    const job = await submit({...body, idempotency_key: pending.get(signature)});
    pending.delete(signature);
    return job;
  };
}

export function sourceForPlayback(item, explicit, previous) {
  if (explicit) return {
    file_id: explicit.source_file_id || explicit.file_id,
    revision: explicit.source_revision || explicit.revision,
  };
  return previous || {file_id: item.file_id, revision: item.revision};
}

// Every asynchronous stage checks ownership before touching the active player.
export class PlaybackSwitches {
  constructor(){this.generation=0;this.pending=false;this.abort=null;}
  cancel(){this.generation++;this.pending=false;this.abort?.abort();this.abort=null;}
  begin(){
    this.cancel();this.pending=true;
    const owner=this,generation=this.generation,controller=new AbortController();this.abort=controller;
    const check=()=>{if(owner.generation!==generation)throw new DOMException('Playback switch cancelled','AbortError');};
    return {
      get current(){return owner.generation===generation;},check,signal:controller.signal,
      onCancel(cleanup){
        if(controller.signal.aborted){cleanup();return ()=>{};}
        controller.signal.addEventListener('abort',cleanup,{once:true});
        return ()=>controller.signal.removeEventListener('abort',cleanup);
      },
      finish(){if(owner.generation===generation)owner.pending=false;},
      delay(ms){return new Promise((resolve,reject)=>{
        check();
        const cancel=()=>{clearTimeout(timer);reject(new DOMException('Playback switch cancelled','AbortError'));};
        const timer=setTimeout(()=>{controller.signal.removeEventListener('abort',cancel);resolve();},ms);
        controller.signal.addEventListener('abort',cancel,{once:true});
      });}
    };
  }
}

export const intendsPlay=state=>state.playbackIntent==='play'&&state.status!=='ended';

// State publication is periodic; native diagnostics expose the current source
// clock. Only use this clock on the diagnosed native route, otherwise retain the
// backend's public state. This does not synchronize two independent decoders.
export function playbackSnapshot(core) {
  const state={...core.state};
  for(const key of ['audioTracks','subtitleTracks'])
    if(state[key])state[key]=state[key].map(track=>({...track}));
  try{
    const backend=core.diagnostics?.backend;
    if(backend?.path==='native'&&Number.isFinite(backend.position)&&backend.position>=0)
      state.currentTime=backend.position;
  }catch{ /* Retain public state if diagnostics are unavailable during recovery. */ }
  return state;
}

// Track ids belong to each engine; preserve the existing language/label match
// while sampling selections again on every final-state reconciliation attempt.
export async function transferTracks(state,to) {
  for(const [key,select] of [['audioTracks','selectAudioTrack'],['subtitleTracks','selectSubtitleTrack']]){
    const selected=state[key]?.find(track=>track.selected);
    const matching=selected&&to.state?.[key]?.find(track=>track.language===selected.language&&track.label===selected.label);
    if(matching&&!matching.selected)await to[select](matching.id);
  }
  if(typeof state.subtitlesVisible==='boolean'&&to.state?.subtitlesVisible!==state.subtitlesVisible)
    await to.subtitleVisible(state.subtitlesVisible);
}

// The replacement is already open and muted. Keep the old engine running until
// session attribution is acknowledged, then transfer the latest transport state.
export async function transferPlayback({from,to,session,selection,previous,check=()=>{},native=null,activate=()=>{}}) {
  let committed=false,mutedOriginal=false,originalMuted;
  let revision=0,lastSeek=null,lastControls;
  const controls=state=>JSON.stringify([state.playbackIntent,state.status==='ended',state.volume,state.muted,state.playbackRate,state.audioTracks,state.subtitleTracks,state.subtitlesVisible]);
  const stop=from.subscribe?.(state=>{
    const next=controls(state),seek=state.pendingOperation;
    if(next!==lastControls){lastControls=next;revision++;}
    if(seek?.kind==='seeking'&&seek.id!==lastSeek){lastSeek=seek.id;revision++;}
  })||(()=>{});
  try {
    const initial=playbackSnapshot(from);
    await to.seek(initial.currentTime);check();
    await session.update(initial.currentTime,intendsPlay(initial)?'playing':'paused',selection);committed=true;
    // Controls and seeks can change during any await, including the final seek
    // or acknowledgement. A bounded retry avoids freezing out a busy user.
    let settled=false;
    for(let attempt=0;attempt<4;attempt++){
      check();
      const state=playbackSnapshot(from),observed=revision;
      if(state.status==='ended')throw new Error('Playback ended before the switch was ready');
      await transferTracks(state,to);
      await to.seek(state.currentTime);
      await to.setPlaybackRate(state.playbackRate);
      await to.setVolume(state.volume);
      if(intendsPlay(state))await to.play();else await to.pause();
      check();
      if(observed!==revision||controls(state)!==controls(from.state))continue;
      // Save the final transport state even when paused: no later timeupdate
      // event is guaranteed to correct the earlier file-switch position.
      await session.update(state.currentTime,intendsPlay(state)?'playing':'paused',selection);
      check();
      if(observed!==revision||controls(state)!==controls(from.state))continue;
      originalMuted=state.muted;settled=true;break;
    }
    if(!settled)throw new Error('Playback changed during the switch. Current playback has been kept; try again.');
    stop();
    if(native&&await native.transfer({from,to,check,activate}))return;
    mutedOriginal=true;
    await from.setMuted(true);
    await to.setMuted(originalMuted);
    activate();
  } catch(error) {
    stop();
    // A cleanup failure must not prevent the remaining rollback operations.
    const failures=[error];
    try{await to.setMuted(true);}catch(failure){failures.push(failure);}
    if(mutedOriginal){try{await from.setMuted(originalMuted);}catch(failure){failures.push(failure);}}
    if(committed){
      try{
        const state=playbackSnapshot(from);
        await session.update(state.currentTime,intendsPlay(state)?'playing':'paused',previous);
        if(state.status==='ended')await session.update(state.currentTime,'ended');
      }catch(failure){failures.push(failure);}
    }
    if(failures.length>1)throw new AggregateError(failures,`Switch failed: ${error.message}. Some rollback operations also failed.`);
    throw error;
  } finally {stop();}
}

// Native video switches wait for matching decoded presentation phases. Audio
// remains owned by Demuxe: claiming its element in another Web Audio graph
// would break later gain/output controls. This is not a sample-gapless mixer.
export class NativeHandoffs {
  video(core){
    const diagnostics=core.diagnostics;
    if(diagnostics?.mode!=='native'||diagnostics.backend?.path!=='native'||diagnostics.backend?.audioProcessing?.component!=='media-element')return null;
    const video=core.host?.querySelector('video');
    return video&&typeof video.requestVideoFrameCallback==='function'?video:null;
  }
  async transfer({from,to,check,activate}){
    const outgoing=this.video(from),incoming=this.video(to);
    if(!outgoing||!incoming)return false;
    const original=playbackSnapshot(from);
    if(!intendsPlay(original))return false;
    const controls=state=>JSON.stringify([state.playbackIntent,state.muted,state.volume,state.playbackRate,state.audioTracks,state.subtitleTracks,state.subtitlesVisible]);
    const signature=controls(original);
    const validate=()=>{
      check();
      if(this.video(from)!==outgoing||this.video(to)!==incoming)throw new Error('Playback surface changed during alignment. Current playback has been kept.');
      if(controls(from.state)!==signature)throw new Error('Playback controls changed during alignment. Current playback has been kept.');
    };
    // The actual unmute follows the matching frame in the same JS task as the
    // surface swap. Demuxe retains ownership of subsequent audio controls.
    await alignNativeFrames(outgoing,incoming,original.playbackRate,validate,()=>{
      validate();outgoing.muted=true;incoming.muted=original.muted;activate();
    });
    // These calls reconcile published state after the synchronous boundary.
    // Activation is irreversible here: a late bookkeeping failure must not
    // roll back the session to the now-hidden outgoing player.
    await Promise.allSettled([from.setMuted(true),to.setMuted(original.muted)]);
    return true;
  }
}

export function alignNativeFrames(from,to,rate,check,activate,{timeoutMs=3000,onSample=()=>{}}={}){
  return new Promise((resolve,reject)=>{
    let oldFrame=null,newFrame=null,oldHandle,newHandle,raf,done=false;
    const initialTime=performance.now(),initialPosition=from.currentTime;
    const cleanup=()=>{clearTimeout(timer);cancelAnimationFrame(raf);if(oldHandle!==undefined)from.cancelVideoFrameCallback(oldHandle);if(newHandle!==undefined)to.cancelVideoFrameCallback(newHandle);to.playbackRate=rate;};
    const finish=error=>{if(done)return;done=true;cleanup();error?reject(error):resolve();};
    const timer=setTimeout(()=>finish(new Error('Could not synchronize replacement frames. Current playback has been kept.')),timeoutMs);
    const oldCallback=(now,meta)=>{oldFrame={...meta,at:now};if(!done)oldHandle=from.requestVideoFrameCallback(oldCallback);};
    const newCallback=(now,meta)=>{newFrame={...meta,at:now};if(!done)newHandle=to.requestVideoFrameCallback(newCallback);};
    oldHandle=from.requestVideoFrameCallback(oldCallback);newHandle=to.requestVideoFrameCallback(newCallback);
    function tick(now){
      try{
        check();
        if(from.paused||from.ended||from.seeking||to.seeking)throw new Error('Playback changed during frame alignment');
        if(Math.abs(from.playbackRate-rate)>.001)throw new Error('Playback speed changed during alignment');
        const expected=initialPosition+(now-initialTime)*rate/1000;
        if(Math.abs(from.currentTime-expected)>.3)throw new Error('Playback position changed during alignment');
        const drift=from.currentTime-to.currentTime;
        // Match presentation phase, not just audio-backed currentTime. Decoder
        // pipelines can present different frames at the same media clock value.
        const phase=oldFrame&&newFrame?(oldFrame.mediaTime-newFrame.mediaTime)-(oldFrame.expectedDisplayTime-newFrame.expectedDisplayTime)*rate/1000:drift;
        onSample({clockDrift:drift,phaseDrift:phase,oldFrame:oldFrame?.mediaTime,newFrame:newFrame?.mediaTime,displayDrift:oldFrame&&newFrame?oldFrame.expectedDisplayTime-newFrame.expectedDisplayTime:null});
        to.playbackRate=Math.max(.1,Math.min(16,rate+Math.max(-.15,Math.min(.15,phase*4))));
        if(oldFrame&&newFrame&&now-oldFrame.at<80&&now-newFrame.at<80&&Math.abs(oldFrame.mediaTime-newFrame.mediaTime)<.0001&&Math.abs(oldFrame.expectedDisplayTime-newFrame.expectedDisplayTime)<3&&Math.abs(drift)<.05){
          to.playbackRate=rate;activate();finish();return;
        }
        raf=requestAnimationFrame(tick);
      }catch(error){finish(error);}
    }
    raf=requestAnimationFrame(tick);
  });
}

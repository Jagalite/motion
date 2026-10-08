// Browser-native handoffs use decoded-frame metadata for the boundary and one
// audio graph for complementary gain changes. Unsupported routes retain the
// conservative handoff; an admitted native alignment must succeed or roll back.
export class NativeHandoffs {
  constructor(){this.context=null;this.entries=new WeakMap();}
  async resume(){
    if(!this.context){this.context=new AudioContext({latencyHint:'interactive'});this.output=this.context.createGain();this.output.connect(this.context.destination);}
    if(this.context.state!=='running')await this.context.resume();
    return this.context.state==='running';
  }
  video(core){
    const diagnostics=core.diagnostics;
    if(diagnostics.mode!=='native'||diagnostics.backend?.path!=='native'||diagnostics.backend?.audioProcessing?.component!=='media-element')return null;
    const video=core.host?.querySelector('video');
    return video&&typeof video.requestVideoFrameCallback==='function'?video:null;
  }
  attach(core,value){
    const video=this.video(core);if(!video)return null;
    let entry=this.entries.get(core);
    if(!entry){
      const gain=this.context.createGain();gain.gain.value=value;
      const source=this.context.createMediaElementSource(video);source.connect(gain);gain.connect(this.output);
      entry={video,source,gain};this.entries.set(core,entry);
    }
    return entry;
  }
  release(core){const entry=this.entries.get(core);if(entry){entry.source.disconnect();entry.gain.disconnect();this.entries.delete(core);}}
  async prepare(from,to){
    if(!this.video(from)||!this.video(to)||!this.entries.has(from))return null;
    if(!await this.resume())return null;
    const outgoing=this.attach(from,1),incoming=this.attach(to,0);
    return {outgoing,incoming};
  }
  async transfer({from,to,check,activate}){
    const pair=await this.prepare(from,to);
    if(!pair)return false;
    const {outgoing,incoming}=pair,original=playbackSnapshot(from);
    const reset=()=>{const now=this.context.currentTime;for(const [entry,value]of [[outgoing,1],[incoming,0]]){entry.gain.gain.cancelScheduledValues(now);entry.gain.gain.setValueAtTime(value,now);}};
    try{
      // The incoming graph is silent even though the media element is unmuted.
      await to.setMuted(original.muted);
      if(intendsPlay(original)){
        await alignNativeFrames(outgoing.video,incoming.video,original.playbackRate,check,()=>{
          check();
          if(!intendsPlay(from.state))throw new Error('Playback intent changed during alignment');
          const time=this.context.currentTime,duration=.008;
          for(const [entry,start,end]of [[outgoing,1,0],[incoming,0,1]]){
            entry.gain.gain.cancelScheduledValues(time);entry.gain.gain.setValueAtTime(start,time);entry.gain.gain.linearRampToValueAtTime(end,time+duration);
          }
          activate();
        },{onSample:sample=>{this.lastAlignment=sample;}});
      }else{reset();outgoing.gain.gain.value=0;incoming.gain.gain.value=1;activate();}
      // Retain the outgoing decoder until the scheduled mix has completed.
      await new Promise(resolve=>setTimeout(resolve,25));
      return true;
    }catch(error){reset();await to.setMuted(true).catch(()=>{});throw error;}
  }
}

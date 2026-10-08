import assert from 'node:assert/strict';
import {alignNativeFrames,NativeHandoffs} from '../web/playback.js';
let frames=new Map(),next=0;
globalThis.requestAnimationFrame=callback=>{frames.set(++next,callback);return next;};
globalThis.cancelAnimationFrame=id=>frames.delete(id);
const video=()=>({currentTime:10,playbackRate:1,paused:false,ended:false,seeking:false,callbacks:new Map(),requestVideoFrameCallback(callback){this.callbacks.set(++next,callback);return next;},cancelVideoFrameCallback(id){this.callbacks.delete(id);},emit(mediaTime,display){const callbacks=[...this.callbacks.values()];this.callbacks.clear();for(const callback of callbacks)callback(performance.now(),{mediaTime,expectedDisplayTime:display});}});
function tick(){const callbacks=[...frames.values()];frames.clear();for(const callback of callbacks)callback(performance.now());}
const old=video(),replacement=video();let activated=0;
const pending=alignNativeFrames(old,replacement,1,()=>{},()=>activated++);
old.emit(10,100);replacement.emit(9.966667,100);tick();
assert.equal(activated,0);assert.ok(replacement.playbackRate>1,'matching clocks must not bypass mismatched frames');
old.emit(10.033333,133);replacement.emit(10.033333,133);tick();await pending;
assert.equal(activated,1);assert.equal(replacement.playbackRate,1);assert.equal(frames.size,0);assert.equal(old.callbacks.size,0);assert.equal(replacement.callbacks.size,0);
const timeout=alignNativeFrames(old,replacement,1,()=>{},()=>activated++,{timeoutMs:5});await assert.rejects(timeout,/Could not synchronize/);assert.equal(activated,1);assert.equal(replacement.playbackRate,1);
const cancel=alignNativeFrames(old,replacement,1,()=>{throw new DOMException('cancelled','AbortError');},()=>activated++);tick();await assert.rejects(cancel,{name:'AbortError'});assert.equal(frames.size,0);
const changed=alignNativeFrames(old,replacement,1,()=>{},()=>activated++);old.paused=true;tick();await assert.rejects(changed,/Playback changed/);old.paused=false;
const manager=new NativeHandoffs();assert.equal(await manager.transfer({from:{diagnostics:{}},to:{diagnostics:{}}}),false);
const makeCore=v=>({diagnostics:{mode:'native',backend:{path:'native',audioProcessing:{component:'media-element'}}},host:{querySelector:()=>v},state:{playbackIntent:'play',volume:1,muted:false,playbackRate:1},async setMuted(value){this.state.muted=value;v.muted=value;}});
const from=makeCore(old),to=makeCore(replacement);replacement.muted=true;
const transfer=manager.transfer({from,to,check:()=>{},activate(){assert.equal(old.muted,true);assert.equal(replacement.muted,false);activated++;}});old.emit(11,200);replacement.emit(11,200);tick();await transfer;
assert.equal(activated,2);assert.equal(from.state.muted,true);assert.equal(to.state.muted,false);
from.state.muted=false;old.muted=false;replacement.muted=true;
const changedControls=manager.transfer({from,to,check:()=>{},activate(){activated++;}});from.state.volume=.5;tick();await assert.rejects(changedControls,/controls changed/);assert.equal(old.muted,false);assert.equal(replacement.muted,true);assert.equal(activated,2);
console.log('Native frame alignment: phase gate, cleanup, timeout, cancellation, control change, synchronous activation PASS');
// A native recovery may replace either surface without changing route metadata.
for(const side of ['outgoing','incoming']){
  const a=video(),b=video(),fresh=video();a.muted=false;b.muted=true;fresh.muted=true;
  const from=makeCore(a),to=makeCore(b);let activated=false;
  const pending=manager.transfer({from,to,check:()=>{},activate(){activated=true;}});
  (side==='outgoing'?from:to).host.querySelector=()=>fresh;
  a.emit(10,100);b.emit(10,100);tick();
  await assert.rejects(pending,/surface changed/);
  assert.equal(activated,false);assert.equal(a.muted,false);assert.equal(b.muted,true);assert.equal(fresh.muted,true);
  assert.equal(a.callbacks.size,0);assert.equal(b.callbacks.size,0);assert.equal(frames.size,0);assert.equal(b.playbackRate,1);
}
console.log('Replaced outgoing/incoming surfaces rejected before activation, cleanup retained: PASS');

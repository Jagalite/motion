// Evaluate this file in the attached Playscale browser before starting playback.
// This reads native decoded pixels; it does not replace media clocks or audio routing.
(() => {
  window.handoffProbe?.stop();
  const canvas=document.createElement('canvas');canvas.width=320;canvas.height=1;
  const context=canvas.getContext('2d',{willReadFrequently:true});
  const ids=new WeakMap(),videos=new WeakSet(),callbacks=new Map();let nextId=0,running=true,raf;
  const receipt={version:1,startedAt:new Date().toISOString(),userAgent:navigator.userAgent,dpr:devicePixelRatio,url:location.href,samples:[],frames:[],marks:[],errors:[]};
  const id=element=>{if(!ids.has(element))ids.set(element,++nextId);return ids.get(element);};
  function video(element){return element?.player?.host?.querySelector('video')||element?.shadowRoot?.querySelector('video');}
  function watch(element,v){
    if(!v||videos.has(v))return;videos.add(v);
    function presented(now,metadata){
      receipt.frames.push({wall:now,element:id(element),active:!element.classList.contains('staging-player')&&element.isConnected,mediaTime:metadata.mediaTime,presentedFrames:metadata.presentedFrames,expectedDisplayTime:metadata.expectedDisplayTime,presentationTime:metadata.presentationTime});
      if(running)callbacks.set(v,v.requestVideoFrameCallback(presented));
    }
    if(v.requestVideoFrameCallback)callbacks.set(v,v.requestVideoFrameCallback(presented));
  }
  function sample(wall){
    const elements=[...document.querySelectorAll('#player-host demuxe-player')];
    for(const e of elements)watch(e,video(e));
    const element=elements.find(e=>!e.classList.contains('staging-player')&&getComputedStyle(e).display!=='none'),v=video(element);
    if(element&&v){
      let frame=null,valid=false;
      try{
        if(v.readyState>=2&&v.videoWidth){
          context.drawImage(v,0,30*v.videoHeight/180,v.videoWidth,v.videoHeight/180,0,0,320,1);
          const pixels=context.getImageData(0,0,320,1).data;
          frame=0;valid=true;
          for(let bit=0;bit<12;bit++){const value=pixels[(bit*24+12)*4];if(value>120)frame|=1<<bit;if(value>80&&value<176)valid=false;}
        }
      }catch(error){receipt.errors.push({wall,message:error.message});}
      receipt.samples.push({wall,element:id(element),frame,valid,mediaTime:v.currentTime,stateTime:element.player.state.currentTime,status:element.player.state.status,readyState:v.readyState,muted:v.muted,volume:v.volume,elements:elements.length});
    }
    if(running)raf=requestAnimationFrame(sample);
  }
  const mutations=new MutationObserver(records=>{
    for(const r of records)if(r.type==='attributes'&&r.target.matches('demuxe-player'))receipt.marks.push({wall:performance.now(),kind:'class',element:id(r.target),value:r.target.className});
  });mutations.observe(document.querySelector('#player-host'),{subtree:true,childList:true,attributes:true,attributeFilter:['class']});
  raf=requestAnimationFrame(sample);
  const probe={receipt,mark(label){receipt.marks.push({wall:performance.now(),kind:'test',label});},stop(){running=false;cancelAnimationFrame(raf);mutations.disconnect();for(const [v,handle]of callbacks)v.cancelVideoFrameCallback(handle);receipt.stoppedAt=new Date().toISOString();return receipt;}};
  window.handoffProbe=probe;
  return {installed:true,userAgent:receipt.userAgent,frameCallbacks:typeof HTMLVideoElement.prototype.requestVideoFrameCallback};
})();

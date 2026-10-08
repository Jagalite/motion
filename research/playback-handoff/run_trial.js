// The native fixture must be open, its local H.264 conversion completed, and
// probe.js installed. This uses the ordinary mode control and unmodified audio.
(() => {
  window.trialDone=false;window.trialError=null;
  window.trialPromise=(async()=>{
    const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
    let player=document.querySelector('demuxe-player:not(.staging-player)');
    await player.player.pause();await player.player.seek(5);
    await player.player.setPlaybackRate(1);await player.player.setVolume(.2);
    await player.player.play();await sleep(1000);
    const probe=window.handoffProbe;
    probe.receipt.route={mode:player.player.diagnostics.mode,backend:player.player.diagnostics.backend.path};
    probe.mark('baseline-steady');await sleep(5000);
    for(const [index,mode]of ['convert','original','convert','original'].entries()){
      probe.mark(`switch-${mode}-${index+1}`);
      const control=document.querySelector('#playback-mode');control.value=mode;
      control.dispatchEvent(new Event('change'));await sleep(7000);
    }
    probe.mark('end');probe.stop();window.trialDone=true;
    await document.querySelector('demuxe-player:not(.staging-player)').player.pause();
  })().catch(error=>{window.trialError=error.message;window.handoffProbe.stop();window.trialDone=true;});
  return {scheduled:true};
})();

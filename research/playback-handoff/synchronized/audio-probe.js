// Install before the first pointer gesture opens a player. Capture the actual
// production mix bus through a silent side branch; do not replace its output.
(() => {
  const connect=AudioNode.prototype.connect;
  window.mixBuses=[];
  AudioNode.prototype.connect=function(destination,...args){
    if(destination instanceof AudioDestinationNode)window.mixBuses.push(this);
    return connect.call(this,destination,...args);
  };
  window.startAudioProbe=async()=>{
    AudioNode.prototype.connect=connect;
    const bus=window.mixBuses[0];if(!bus)throw new Error('No production mix bus observed');
    const context=bus.context;
    const source=`class OutputProbe extends AudioWorkletProcessor{
      constructor(){super();this.batch=[];this.previous=0;this.run=0;this.port.onmessage=()=>{this.port.postMessage(this.batch);this.batch=[];};}
      process(inputs){const input=inputs[0]?.[0];if(!input)return true;let sum=0,peak=0,maxStep=0,maxZeroRun=0;
        for(const value of input){sum+=value*value;peak=Math.max(peak,Math.abs(value));maxStep=Math.max(maxStep,Math.abs(value-this.previous));this.previous=value;this.run=Math.abs(value)<1e-7?this.run+1:0;maxZeroRun=Math.max(maxZeroRun,this.run);}
        this.batch.push({frame:currentFrame,time:currentTime,length:input.length,rms:Math.sqrt(sum/input.length),peak,maxStep,maxZeroRun});
        if(this.batch.length>=16){this.port.postMessage(this.batch);this.batch=[];}return true;
      }}registerProcessor('playscale-output-probe',OutputProbe);`;
    const url=URL.createObjectURL(new Blob([source],{type:'text/javascript'}));
    try{await context.audioWorklet.addModule(url);}finally{URL.revokeObjectURL(url);}
    const tap=new AudioWorkletNode(context,'playscale-output-probe'),silent=context.createGain();silent.gain.value=0;
    bus.connect(tap);tap.connect(silent);silent.connect(context.destination);
    const receipt={sampleRate:context.sampleRate,baseLatency:context.baseLatency,outputLatency:context.outputLatency,contextTime:context.currentTime,performanceTime:performance.now(),kind:'production-postmix-PCM-before-device',blocks:[]};
    tap.port.onmessage=event=>receipt.blocks.push(...event.data);
    window.audioProbe={receipt,context,stop(){tap.port.postMessage('flush');setTimeout(()=>{bus.disconnect(tap);tap.disconnect();silent.disconnect();},50);return receipt;}};
    return {sampleRate:context.sampleRate,state:context.state,buses:window.mixBuses.length};
  };
  return {installed:true};
})();

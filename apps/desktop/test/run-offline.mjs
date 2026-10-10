// Cold-start the production desktop/helper with a private A13 cache fixture.
import {execFileSync, spawn} from 'node:child_process';
import {mkdtempSync, mkdirSync, readdirSync, readFileSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import electron from 'electron';
import {createOwnedServer} from '../src/owned.mjs';
const root=fileURLToPath(new URL('../../../',import.meta.url));
const work=mkdtempSync(join(tmpdir(),'motion-offline-proof-'));
execFileSync(process.env.MOTION_CARGO_BINARY || 'cargo',['build','--locked','-p','motion-ui-host'],{cwd:root,stdio:'inherit'});
const target=process.env.CARGO_TARGET_DIR ? resolve(root,process.env.CARGO_TARGET_DIR) : join(root,'target');
const helper=join(work,'motion-ui-host');
const helperBytes=readFileSync(join(target,'debug/motion-ui-host'));
writeFileSync(helper,helperBytes,{mode:0o700});
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const hashes={};
for(const relative of [...readdirSync(join(root,'apps/desktop/src')).map(name=>`apps/desktop/src/${name}`),'apps/desktop/test/offline-smoke.mjs','contracts/Motion_Server_API_v2.yaml']){
 const bytes=readFileSync(join(root,relative));const destination=join(work,relative);mkdirSync(join(destination,'..'),{recursive:true});writeFileSync(destination,bytes);hashes[relative]=sha(bytes);
}
const cache=join(work,'cache');mkdirSync(join(cache,'blobs'),{recursive:true});
const media=join(work,'sample.mp4');
execFileSync('ffmpeg',['-hide_banner','-loglevel','error','-f','lavfi','-i','testsrc2=size=640x360:rate=24:duration=12','-f','lavfi','-i','sine=frequency=440:duration=12','-c:v','libx264','-pix_fmt','yuv420p','-c:a','aac','-movflags','+faststart','-shortest',media]);
const bytes=readFileSync(media);const digest=sha(bytes);writeFileSync(join(cache,'blobs',digest),bytes);
const manifest={protocol:1,scope:{server_id:'offline-fixture',principal_id:'principal',profile_id:'profile',device_id:'device'},downloads:[{identity:{download_id:'sample',timeline_id:'timeline',timeline_revision:'1',source_revision:'source1',base_viewing_revision:'4',base_manual_epoch:'2'},title:'Offline cold-start sample',duration_ms:12000,sha256:digest,size:bytes.length,content_type:'video/mp4'}]};
writeFileSync(join(cache,'manifest.json'),JSON.stringify(manifest));
const output=join(work,'receipt.json');
const env={...process.env,MOTION_CACHE_DIR:cache,MOTION_UI_HOST_BINARY:helper,MOTION_OFFLINE_RECEIPT:output};delete env.ELECTRON_RUN_AS_NODE;
const runtime=process.env.MOTION_ELECTRON_BINARY || electron;
const child=spawn(runtime,[join(work,'apps/desktop/test/offline-smoke.mjs')],{env,stdio:'inherit'});
const timer=setTimeout(()=>child.kill('SIGKILL'),270000);
const code=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',resolve)}).finally(()=>clearTimeout(timer));
let receipt;try{receipt=JSON.parse(readFileSync(output,'utf8'))}catch{receipt={passed:false,failure:'Offline smoke exited before receipt',checks:{}}}
if (receipt.passed) {
  let child;
  const owner=createOwnedServer({presentationOnly:true,executable:helper,dataDir:cache,
    demuxeDir:resolve(process.env.MOTION_DEMUXE_DIR || join(root,'web/vendor/demuxe')),
    launch:(...args)=>{child=spawn(...args);return child;}});
  try {
    await owner.start();
    const exited=new Promise((resolve,reject)=>{const deadline=setTimeout(()=>reject(new Error('Offline helper survived parent-pipe loss')),5000);child.once('exit',()=>{clearTimeout(deadline);resolve()})});
    child.stdio[5].end();
    await exited;
    receipt.checks.parent_pipe_loss_terminates_helper=true;
  } catch(error) {receipt.passed=false;receipt.failure=String(error.stack??error)}
  finally {await owner.stop()}
}
Object.assign(receipt,{source_sha256:hashes,helper_sha256:sha(helperBytes),runtime_binary_sha256:sha(readFileSync(runtime)),media_sha256:digest,cache_protocol:1,
 commit:execFileSync('git',['rev-parse','HEAD'],{cwd:root}).toString().trim(),recorded_at:new Date().toISOString(),artifacts:work,
 limitations:['Synthetic A13 cache manifest; download transfer and remote reconciliation are not exercised.','macOS arm64 unpackaged Electron only; audio muted; no screen-reader or physical A/V observations.']});
const file=join(root,'qualification/desktop/offline-cold-start.json');writeFileSync(file,JSON.stringify(receipt,null,2)+'\n');console.log(file);
process.exit(code===0&&receipt.passed?0:1);

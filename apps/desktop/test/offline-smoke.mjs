import {app,BrowserWindow,webContents} from 'electron';
import {mkdtempSync,readFileSync,writeFileSync,existsSync} from 'node:fs';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import assert from 'node:assert/strict';
const work=mkdtempSync(join(tmpdir(),'motion-offline-native-'));app.setPath('userData',join(work,'user-data'));
const checks={};let failure;let finished=false;
const timeout=setTimeout(()=>{failure='Offline native proof exceeded 240 seconds';finish()},240000);
function finish(){if(finished)return;finished=true;clearTimeout(timeout);writeFileSync(process.env.MOTION_OFFLINE_RECEIPT,JSON.stringify({passed:!failure,failure,checks,scope:'Real offline presentation helper and production desktop, synthetic cache; no authoritative server',electron:process.versions.electron,chromium:process.versions.chrome},null,2));app.exit(failure?1:0)}
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn){for(let i=0;i<600;i++){const value=await fn();if(value)return value;await sleep(100)}throw new Error('Timed out waiting for offline condition')}
async function playerReady(remote) {
 await until(async()=>{
  const state=await remote.executeJavaScript('({state:document.querySelector("#motion-offline-player")?.dataset.state,message:document.querySelector("#offline-progress")?.textContent})');
  if(state.state==='failed') throw new Error(state.message);
  return state.state==='ready';
 });
}
app.on('browser-window-created',(_,window)=>window.hide());
void (async () => {
try{
 await import('../src/main.mjs');await app.whenReady(); console.log('Offline checkpoint: app ready');
 const window=await until(()=>{const w=BrowserWindow.getAllWindows()[0];return w&&!w.webContents.isLoading()&&w.webContents.getURL().endsWith('/chrome.html')?w:null});
 await window.webContents.executeJavaScript('window.motionHost.openDownloads()');
 let remote=webContents.getAllWebContents().find(w=>w!==window.webContents&&w.getURL().startsWith('http://127.0.0.1:'));
 assert.ok(remote);const origin=new URL(remote.getURL()).origin;
 checks.cold_start_without_authoritative_server=await remote.executeJavaScript('document.querySelector("h1").textContent === "Downloaded titles"');assert.equal(checks.cold_start_without_authoritative_server,true);
 assert.equal(await remote.executeJavaScript('typeof require === "undefined" && typeof motionHost === "undefined"'),true);checks.sandboxed=true;
 await remote.loadURL(`${origin}/download/sample`);
 await playerReady(remote);console.log('Offline checkpoint: player ready');
 await remote.executeJavaScript('document.querySelector("demuxe-player").setMuted(true).then(()=>document.querySelector("demuxe-player").play())');
 await until(()=>remote.executeJavaScript('document.querySelector("demuxe-player").player.state.currentTime > 2.5'));
 checks.real_offline_playback=true;
 await remote.executeJavaScript('document.querySelector("demuxe-player").seek(7)');
 await until(()=>remote.executeJavaScript('document.querySelector("demuxe-player").player.state.currentTime > 7.2'));
 checks.offline_seek=true;
 await window.webContents.executeJavaScript('window.motionHost.disconnect()');
 assert.ok(remote.isDestroyed());
 await assert.rejects(fetch(origin,{signal:AbortSignal.timeout(2000)}));checks.owned_helper_stopped=true;
 let events=JSON.parse(readFileSync(join(process.env.MOTION_CACHE_DIR,'events.json'),'utf8'));
 assert.ok(events.length>0);assert.ok(events.at(-1).position_ms>=7000);assert.equal(events.at(-1).media.base_manual_epoch,'2');assert.ok(events.every((e,i)=>e.device_sequence===String(i+1)));checks.durable_ordered_progress=true;
 await window.webContents.executeJavaScript('window.motionHost.openDownloads()');
 remote=webContents.getAllWebContents().find(w=>w!==window.webContents&&w.getURL().startsWith('http://127.0.0.1:'));
 const second=new URL(remote.getURL()).origin;
 await remote.loadURL(`${second}/download/sample`);await playerReady(remote);console.log('Offline checkpoint: restarted player ready');
 assert.ok(await remote.executeJavaScript('document.querySelector("demuxe-player").player.state.currentTime >= 7'));checks.restart_resumes_from_cache=true;
 await window.webContents.executeJavaScript('window.motionHost.disconnect()');
 assert.equal(existsSync(join(process.env.MOTION_CACHE_DIR,'motion.sqlite')),false);assert.equal(existsSync(join(work,'user-data/server')),false);checks.no_authoritative_database_created=true;
}catch(error){failure=String(error.stack??error);console.error(failure)}
finish();
})();

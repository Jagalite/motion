// Player checks for Motion's HLS output: plays the server's exact HLS output
// (written by the `MOTION_DUMP_HLS` option of tests/v2_playback.rs) with a
// given Shaka build in headless Chromium. Default: selects the in-manifest
// subtitle track and asserts the active cue at media times. `--abr`: plays an
// adaptive ladder while DevTools throttles the network, and asserts the low
// rung is chosen while throttled and the main rung after the network is
// restored. These check the output format against the player; they are not a
// substitute for the production client matrix.
//
//   MOTION_DUMP_HLS=<dir> cargo test -p playscale --test v2_playback selected_plain_text
//   MOTION_PLAYWRIGHT_DIR=<dir containing node_modules/playwright> \
//   node scripts/check_hls_subtitles.mjs --shaka <shaka-player.js> \
//     --dump <dir>/<case> --expect 0.5=Selected\ caption --expect 6=
import {createRequire} from 'node:module';
import {createServer} from 'node:http';
import {readFileSync} from 'node:fs';
import {join, extname, resolve} from 'node:path';

const args = process.argv.slice(2);
const value = name => args[args.indexOf(name) + 1];
const expectations = args.flatMap((a, i) => a === '--expect' ? [args[i + 1]] : [])
  .map(e => { const at = e.indexOf('='); return {t: Number(e.slice(0, at)), text: e.slice(at + 1)}; });
const dump = resolve(value('--dump'));
const shaka = readFileSync(resolve(value('--shaka')));
const require = createRequire(join(resolve(process.env.MOTION_PLAYWRIGHT_DIR ?? '.'), 'node_modules/'));
const {chromium} = require('playwright');
const types = {'.m3u8': 'application/vnd.apple.mpegurl', '.m4s': 'video/iso.segment', '.mp4': 'video/mp4', '.vtt': 'text/vtt'};
const server = createServer((req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (path === '/') { res.writeHead(200, {'content-type': 'text/html'}); res.end('<!doctype html><video id=v muted playsinline></video><script src="/shaka.js"></script>'); return; }
  if (path === '/shaka.js') { res.writeHead(200, {'content-type': 'text/javascript'}); res.end(shaka); return; }
  try { const file = join(dump, path); if (!file.startsWith(dump)) throw new Error('outside');
    res.writeHead(200, {'content-type': types[extname(file)] ?? 'application/octet-stream'}); res.end(readFileSync(file)); }
  catch { res.writeHead(404); res.end(); }
}).listen(0, '127.0.0.1');
const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  if (args.includes('--abr')) {
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Network.enable');
    const throttle = kbps => cdp.send('Network.emulateNetworkConditions', {offline: false, latency: 20,
      downloadThroughput: kbps ? kbps * 1000 / 8 : -1, uploadThroughput: -1});
    await throttle(600);
    await page.evaluate(async () => {
      const video = document.getElementById('v');
      shaka.polyfill.installAll();
      const player = new shaka.Player();
      await player.attach(video);
      window.__errors = [];
      player.addEventListener('error', e => window.__errors.push(e.detail?.code));
      player.configure({abr: {enabled: true, switchInterval: 1, defaultBandwidthEstimate: 500000},
        streaming: {bufferingGoal: 4, rebufferingGoal: 1}});
      window.__player = player;
      await player.load('/master.m3u8');
      await video.play();
    });
    const active = () => page.evaluate(() => ({height: window.__player.getVariantTracks().find(t => t.active)?.height,
      heights: [...new Set(window.__player.getVariantTracks().map(t => t.height))], errors: window.__errors,
      estimate: Math.round(window.__player.getStats().estimatedBandwidth)}));
    await new Promise(r => setTimeout(r, 6000));
    const throttled = await active();
    await throttle(0);
    let restored = await active();
    for (let i = 0; i < 20 && restored.height !== Math.max(...restored.heights); i++) {
      await new Promise(r => setTimeout(r, 1000));
      restored = await active();
    }
    console.log(JSON.stringify({throttled, restored}));
    const low = Math.min(...throttled.heights), high = Math.max(...throttled.heights);
    const ok = throttled.heights.length === 2 && throttled.height === low && restored.height === high
      && throttled.errors.length === 0 && restored.errors.length === 0;
    if (!ok) console.error('adaptive switching not observed as expected');
    process.exitCode = ok ? 0 : 1;
  } else {
  const observed = await page.evaluate(async times => {
    const video = document.getElementById('v');
    shaka.polyfill.installAll();
    const player = new shaka.Player();
    await player.attach(video);
    const errors = [];
    player.addEventListener('error', e => errors.push(e.detail?.code));
    await player.load('/master.m3u8');
    const texts = player.getTextTracks();
    if (texts.length !== 1) return {errors, tracks: texts.length};
    player.selectTextTrack(texts[0]);
    // Shaka settles its start position after load; a seek before that is
    // overridden.
    await new Promise(r => setTimeout(r, 1500));
    const out = [];
    for (const t of times) {
      video.currentTime = t;
      await new Promise(r => setTimeout(r, 1500));
      out.push({t, current: video.currentTime, cues: [...video.textTracks].flatMap(tr => [...(tr.activeCues ?? [])].map(c => c.text)).join('|')});
    }
    return {errors, tracks: texts.length, live: player.isLive(), out};
  }, expectations.map(e => e.t));
  console.log(JSON.stringify(observed));
  let failed = observed.errors.length > 0 || observed.tracks !== 1;
  for (const [i, e] of expectations.entries()) {
    const got = observed.out?.[i];
    if (!got || Math.abs(got.current - e.t) > 0.25 || got.cues !== e.text) {
      failed = true;
      console.error(`at ${e.t}s expected ${JSON.stringify(e.text)}, observed ${JSON.stringify(got)}`);
    }
  }
  process.exitCode = failed ? 1 : 0;
  }
} finally {
  await browser.close();
  server.close();
}

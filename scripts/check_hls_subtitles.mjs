// Player check for Motion's live HLS subtitle rendition: plays the server's
// exact HLS output (written by the `MOTION_DUMP_HLS` option of
// tests/v2_playback.rs) with a given Shaka build in headless Chromium,
// selects the in-manifest subtitle track and asserts the active cue at media
// times. This checks the rendition format against the player; it is not a
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
} finally {
  await browser.close();
  server.close();
}

// Disposable browser regression server. No user libraries or production service.
import {createServer} from 'node:http';
import {readFile, mkdir} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
const assets = path.resolve(process.env.DEMUXE_DIR ?? path.join(root, 'web/vendor/demuxe'));
const fixtures = path.join(root, 'artifacts/demuxe-session');
await mkdir(fixtures, {recursive: true});
const fixture = path.join(fixtures, 'fixture.mp4');
const generated = spawnSync('ffmpeg', ['-y', '-v', 'error', '-f', 'lavfi', '-i',
  'testsrc2=size=320x180:rate=24', '-t', '12', '-c:v', 'libx264', '-threads', '2',
  '-pix_fmt', 'yuv420p', '-movflags', '+faststart', fixture]);
if (generated.status !== 0) throw Error(generated.stderr.toString());
const wasm = Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const wasmPath = 'web/engine-remux/remux.wasm';
const identity = 'sha256:' + hash(JSON.stringify({['runtime/' + wasmPath]: hash(wasm)}, null, 2) + '\n');
const extra = {schema: 1, providerContractVersion: 1, revision: 'cache-fixture',
  assets: [{id: 'cache-fixture', path: wasmPath, bytes: wasm.length, sha256: hash(wasm)}],
  providers: [{id: 'ffmpeg-file-preparation', implementationIdentity: identity, technology: 'wasm',
    delivery: ['optional-assets'], assetIds: ['cache-fixture'],
    offers: [{capability: 'media.prepare.file', version: 1, profile: 'packet-copy'}]}]};
const requests = {};
let failManifest = true;
createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const name = url.pathname;
  if (name !== '/requests') requests[name] = (requests[name] ?? 0) + 1;
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  try {
    let bytes, type = 'text/javascript';
    if (name === '/') {
      bytes = Buffer.from('<!doctype html><title>Playscale Demuxe session tests</title><h1>Playscale Demuxe session tests</h1>');
      type = 'text/html';
    } else if (name === '/requests') {
      bytes = Buffer.from(JSON.stringify(requests)); type = 'application/json';
    } else if (name === '/test-config') {
      bytes = Buffer.from(JSON.stringify({qualified: {'ffmpeg-file-preparation': identity}})); type = 'application/json';
    } else if (name === '/assets/demuxe/cache-fixture.json') {
      bytes = Buffer.from(JSON.stringify(extra)); type = 'application/json';
    } else if (name === '/assets/demuxe/' + wasmPath) {
      bytes = wasm; type = 'application/wasm';
    } else if (name === '/fixture.mp4' || name === '/slow.mp4') {
      if (name === '/slow.mp4') await new Promise(resolve => setTimeout(resolve, 400));
      bytes = await readFile(fixture); type = 'video/mp4';
    } else if (name === '/session-tests.js') {
      bytes = await readFile(path.join(root, 'scripts/demuxe_session_browser.js'));
    } else if (name === '/demuxe.js') {
      bytes = await readFile(path.join(root, 'web/demuxe.js'));
    } else if (name.startsWith('/assets/demuxe/')) {
      if (name.endsWith('/demuxe-providers.json') && failManifest) {
        failManifest = false; res.writeHead(503).end(); return;
      }
      const file = path.resolve(assets, '.' + name.slice('/assets/demuxe'.length));
      if (!file.startsWith(assets + path.sep)) throw Error('Invalid path');
      bytes = await readFile(file);
      if (name.endsWith('.json')) type = 'application/json';
    } else { res.writeHead(404).end(); return; }
    res.setHeader('Content-Type', type);
    res.setHeader('ETag', '"' + hash(bytes) + '"');
    res.setHeader('Accept-Ranges', 'bytes');
    const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range ?? '');
    if (range) {
      const start = +range[1], end = range[2] ? Math.min(+range[2], bytes.length - 1) : bytes.length - 1;
      res.writeHead(206, {'Content-Range': `bytes ${start}-${end}/${bytes.length}`, 'Content-Length': end-start+1});
      res.end(bytes.subarray(start, end + 1));
    } else { res.setHeader('Content-Length', bytes.length); res.end(req.method === 'HEAD' ? undefined : bytes); }
  } catch { res.writeHead(404).end(); }
}).listen(Number(process.env.PORT ?? 4189), '127.0.0.1', () => console.log('Demuxe session tests: http://127.0.0.1:' + (process.env.PORT ?? 4189)));

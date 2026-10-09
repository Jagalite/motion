// Run the Wave 0 desktop proof: start the proof server (real motion-ui
// composition, mock services), run Electron against its loopback origin,
// and write a qualification receipt.
//   cargo +1.98.0 build -p motion-ui --example proof_server
//   npm run proof -w apps/desktop   (or: node apps/desktop/proof/run.mjs)
import {execFileSync, spawn} from 'node:child_process';
import {createHash} from 'node:crypto';
import {existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {mkdtemp} from 'node:fs/promises';
import {release, tmpdir} from 'node:os';
import {join} from 'node:path';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
import electron from 'electron';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const server = join(root, 'target/debug/examples/proof_server');
const demuxe = process.env.MOTION_DEMUXE_DIR ?? join(root, 'web/vendor/demuxe');
if (!existsSync(server)) throw new Error(`build first: cargo +1.98.0 build -p motion-ui --example proof_server`);
if (!existsSync(join(demuxe, 'package.json'))) throw new Error(`Demuxe package not found at ${demuxe}; set MOTION_DEMUXE_DIR`);

const work = await mkdtemp(join(tmpdir(), 'motion-desktop-proof-'));
const media = join(work, 'sample.mp4');
// 12 s H.264 High/AAC-LC SDR, faststart, generated locally (no third-party media).
execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=24:duration=12',
  '-f', 'lavfi', '-i', 'sine=frequency=440:duration=12', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-profile:v', 'high',
  '-c:a', 'aac', '-b:a', '96k', '-movflags', '+faststart', '-shortest', media]);
const mediaSha = createHash('sha256').update(readFileSync(media)).digest('hex');

const child = spawn(server, ['--media', media, '--demuxe-dir', demuxe], {stdio: ['ignore', 'pipe', 'inherit']});
const requests = [];
const lines = createInterface({input: child.stdout});
const announced = await new Promise((resolve, reject) => {
  lines.on('line', line => {
    const message = JSON.parse(line);
    if (message.listening) resolve(message);
    else if (message.request) requests.push(message.request);
  });
  child.once('exit', code => reject(new Error(`proof server exited ${code}`)));
});

const output = join(work, 'observed.json');
// A parent that is itself an Electron app may export ELECTRON_RUN_AS_NODE,
// which would run this proof as plain Node instead of Electron.
const electronEnv = {...process.env};
delete electronEnv.ELECTRON_RUN_AS_NODE;
await new Promise((resolve, reject) => {
  const proc = spawn(electron, [fileURLToPath(new URL('./main.mjs', import.meta.url))], {
    stdio: 'inherit',
    // The bootstrap travels over this private environment of a child process the launcher owns;
    // the packaged host will use an inherited pipe (plan 12.2).
    env: {...electronEnv, MOTION_PROOF_ORIGIN: announced.listening, MOTION_PROOF_BOOTSTRAP: announced.bootstrap, MOTION_PROOF_OUT: output},
  });
  proc.once('exit', code => (code === 0 ? resolve() : reject(new Error(`electron exited ${code}`))));
});
await new Promise(r => setTimeout(r, 300));
child.kill('SIGTERM');

const observed = JSON.parse(readFileSync(output, 'utf8'));
const c = observed.checks;
const deliveriesCreated = requests.filter(r => r.method === 'POST' && r.path === '/api/v2/playback/delivery-sessions' && r.status === 201).length;
const deliveriesClosed = requests.filter(r => r.method === 'DELETE' && r.path.startsWith('/api/v2/playback/delivery-sessions/') && r.status === 204).length;
const results = {
  session_cookie_http_only: c.sessionCookie?.some(x => x.name === 'motion_session' && x.httpOnly) ?? false,
  bootstrap_one_use: c.bootstrapReplayStatus >= 400 && c.bootstrapReplayStatus < 500,
  strict_csp_served: typeof c.pageCsp === 'string' && !c.pageCsp.includes("'unsafe-eval'") && !c.pageCsp.includes("'unsafe-inline'"),
  no_node_in_page: c.nodeAvailable === false,
  eval_blocked_by_csp: c.evalBlocked === 'EvalError',
  inline_script_blocked: c.inlineScriptBlocked === true,
  player_ready: c.playerHost?.state === 'ready',
  played_past_2_5s: c.playback?.played === true,
  seek_to_7s_played: c.playback?.sought === true,
  range_requests: requests.filter(r => r.range && r.path.startsWith('/api/v2/media/')).length > 0,
  delivery_closed_on_leave: deliveriesCreated > 0 && deliveriesClosed === deliveriesCreated,
  waited_for_starting_generation: requests.some(r => r.method === 'GET' && /^\/api\/v2\/playback\/delivery-sessions\/[^/]+$/.test(r.path) && r.status === 200),
  replays_after_back_navigation: c.afterBack?.state === 'ready' && c.afterBack?.players === 1 && deliveriesCreated === 2,
};
const passed = Object.values(results).every(Boolean) && !c.exception;
const demuxePkg = JSON.parse(readFileSync(join(demuxe, 'package.json'), 'utf8'));
const demuxeInstall = existsSync(join(demuxe, 'playscale-package.json')) ? JSON.parse(readFileSync(join(demuxe, 'playscale-package.json'), 'utf8')) : {};
const receipt = {
  kind: 'topcoat-electron-local-origin-proof',
  wave: 0,
  passed,
  recorded_at: new Date().toISOString(),
  motion_commit: execFileSync('git', ['rev-parse', 'HEAD'], {cwd: root}).toString().trim(),
  worktree_dirty: execFileSync('git', ['status', '--porcelain', '--', '.', ':!qualification/desktop'], {cwd: root}).toString().trim().length > 0,
  environment: {electron: observed.electron, chromium: observed.chromium, node: observed.node, os: `${process.platform} ${release()}`, arch: observed.arch},
  topcoat: 'tokio-rs/topcoat 341f3ff2fe16a73af5469685cf597693af5acb25, features router+view+tower+discover (no runtime)',
  origin: 'single loopback origin serving Topcoat HTML, /ui assets, /assets/demuxe, mock /api/v2 and media',
  demuxe: {name: demuxePkg.name, version: demuxePkg.version, archive_sha256: demuxeInstall.archive_sha256 ?? null},
  media: {generator: 'ffmpeg lavfi testsrc2+sine, H.264 High/AAC-LC, 640x360@24, 12 s, faststart', sha256: mediaSha},
  results,
  observations: c,
  csp_console: observed.console.filter(line => /Content Security Policy|Refused/i.test(line)),
  permission_requests: observed.permissionRequests,
  http: {requests: requests.length, deliveries_created: deliveriesCreated, deliveries_closed: deliveriesClosed},
  limitations: [
    'Proof server uses a mock UiQueryFacade, mock session exchange and mock playback routes; it is not the Motion server.',
    'Bootstrap passed through the child-process environment of the launcher, not yet an inherited pipe.',
    'Native route of the installed reduced Demuxe package only; no Wasm/WebCodecs providers are installed.',
    'Audio muted; audible output, A/V sync, colour and HDR not observed. Hidden window, unpackaged, unsigned.',
    'Wave 0 player bridge: no viewing authority, progress events, generation switching or lease renewal.',
  ],
};
const dir = join(root, 'qualification', 'desktop');
mkdirSync(dir, {recursive: true});
const file = join(dir, `topcoat-electron-proof-${process.platform}-${observed.arch}.json`);
writeFileSync(file, JSON.stringify(receipt, null, 2) + '\n');
console.log(`${passed ? 'PASSED' : 'FAILED'}: ${file}`);
console.log(JSON.stringify(results, null, 2));
process.exit(passed ? 0 : 1);

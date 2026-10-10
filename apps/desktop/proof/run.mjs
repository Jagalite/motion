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
if (!existsSync(join(demuxe, 'package.json'))) throw new Error(`Demuxe package not found at ${demuxe}; set MOTION_DEMUXE_DIR`);
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

// Bind the receipt to what actually ran: build the proof server from this tree
// now, and verify every installed Demuxe file against its install receipt.
execFileSync('rustup', ['run', '1.98.0', 'cargo', 'build', '--locked', '-p', 'motion-ui', '--example', 'proof_server'], {cwd: root, stdio: 'inherit'});
const serverSha = sha256(readFileSync(server));
const lock = readFileSync(join(root, 'Cargo.lock'), 'utf8');
const topcoatSource = /name = "topcoat"\nversion = "([^"]+)"\nsource = "([^"]+)"/.exec(lock);
const installReceipt = JSON.parse(readFileSync(join(demuxe, 'playscale-package.json'), 'utf8'));
const demuxeMismatches = Object.entries(installReceipt.files ?? {})
  .filter(([file, digest]) => !existsSync(join(demuxe, file)) || sha256(readFileSync(join(demuxe, file))) !== digest)
  .map(([file]) => file);

const work = await mkdtemp(join(tmpdir(), 'motion-desktop-proof-'));
// Launch identical bytes from the host temporary volume: macOS can stall in
// its loader when Node launches a freshly built executable on an external disk.
const launchServer = join(work, 'proof_server');
// Write bytes into a new file instead of cloning source filesystem metadata.
writeFileSync(launchServer, readFileSync(server), {mode: 0o700, flag: 'wx'});
const launchServerSha = sha256(readFileSync(launchServer));
if (launchServerSha !== serverSha) throw new Error('Temporary proof server differs from the built executable');
const media = join(work, 'sample.mp4');
// 12 s H.264 High/AAC-LC SDR, faststart, generated locally (no third-party media).
execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=24:duration=12',
  '-f', 'lavfi', '-i', 'sine=frequency=440:duration=12', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-profile:v', 'high',
  '-c:a', 'aac', '-b:a', '96k', '-movflags', '+faststart', '-shortest', media]);
const mediaSha = createHash('sha256').update(readFileSync(media)).digest('hex');

const requests = [];
const admitted = [];
const commands = [];
const closed = [];
const output = join(work, 'observed.json');
let child;
let electronChild;
let lines;
async function stop(proc) {
  if (!proc?.pid || proc.exitCode !== null || proc.signalCode !== null) return;
  await new Promise(resolve => {
    const timer = setTimeout(() => proc.kill('SIGKILL'), 2000);
    proc.once('exit', () => { clearTimeout(timer); resolve(); });
    proc.kill('SIGTERM');
  });
}
try {
  child = spawn(launchServer, ['--media', media, '--demuxe-dir', demuxe], {stdio: ['ignore', 'pipe', 'inherit']});
  lines = createInterface({input: child.stdout});
  const announced = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Proof server did not announce readiness within 30 seconds')), 30000);
    const fail = error => { clearTimeout(timer); reject(error); };
    lines.on('line', line => {
      let message;
      try { message = JSON.parse(line); } catch { fail(new Error('Invalid proof server protocol output')); return; }
      if (message.listening) { clearTimeout(timer); resolve(message); }
      else if (message.request) requests.push(message.request);
      else if (message.delivery_admitted) admitted.push(message.delivery_admitted);
      else if (message.delivery_closed) closed.push(message.delivery_closed);
      else if (message.command) commands.push(message.command);
    });
    child.once('error', fail);
    child.once('exit', code => fail(new Error(`proof server exited ${code}`)));
  });
  console.log('Proof server ready; launching Electron');

  // A parent Electron app may export this flag, causing plain Node execution.
  const electronEnv = {...process.env};
  delete electronEnv.ELECTRON_RUN_AS_NODE;
  await new Promise((resolve, reject) => {
    electronChild = spawn(electron, [fileURLToPath(new URL('./main.mjs', import.meta.url))], {
      stdio: 'inherit',
      // Private child environment for this proof; packaged host will use a pipe.
      env: {...electronEnv, MOTION_PROOF_ORIGIN: announced.listening, MOTION_PROOF_BOOTSTRAP: announced.bootstrap, MOTION_PROOF_OUT: output},
    });
    const timer = setTimeout(() => reject(new Error('Electron proof exceeded 120 seconds')), 120000);
    electronChild.once('error', error => { clearTimeout(timer); reject(error); });
    electronChild.once('exit', code => {
      clearTimeout(timer);
      code === 0 ? resolve() : reject(new Error(`electron exited ${code}`));
    });
  });
  await new Promise(r => setTimeout(r, 300));
} finally {
  await stop(electronChild);
  await stop(child);
  lines?.close();
}

const observed = JSON.parse(readFileSync(output, 'utf8'));
const c = observed.checks;
const deliveriesCreated = admitted.length;
const deliveriesClosed = closed.length;
const results = {
  session_cookie_http_only: c.sessionCookie?.some(x => x.name === 'motion_session' && x.httpOnly) ?? false,
  bootstrap_one_use: c.bootstrapReplay?.status === 401 && c.bootstrapReplay?.code === 'unauthenticated',
  strict_csp_served: typeof c.pageCsp === 'string' && !c.pageCsp.includes("'unsafe-eval'") && !c.pageCsp.includes("'unsafe-inline'"),
  no_node_in_page: c.nodeAvailable === false,
  eval_blocked_by_csp: c.evalBlocked === 'EvalError',
  inline_script_blocked: c.inlineScriptBlocked === true,
  player_ready: c.playerHost?.state === 'ready',
  played_past_2_5s: c.playback?.played === true,
  seek_landed_and_continued: c.playback?.landed === true && c.playback?.advancing === true,
  range_requests: requests.filter(r => r.range && r.path.startsWith('/api/v2/media/')).length > 0,
  // Every admitted delivery closed exactly once (by identity), none left open.
  every_delivery_retired: admitted.length > 0 && admitted.length === new Set(admitted).size
    && closed.length === admitted.length && admitted.every(id => closed.includes(id)),
  waited_for_starting_generation: requests.some(r => r.method === 'GET' && /^\/api\/v2\/playback\/delivery-sessions\/[^/]+$/.test(r.path) && r.status === 200),
  active_delivery_lease_renewed: commands.some(cmd => cmd.operation === 'heartbeatDelivery'
    && cmd.delivery === admitted[0] && cmd.body?.active_generation === '1'),
  plays_after_back_navigation: c.afterBack?.state === 'ready' && c.afterBack?.players === 1 && c.afterBackPlayback?.advancing === true && deliveriesCreated === 2,
  // A command form reached the public API with a 32-hex idempotency key and the typed body.
  command_form_creates_scan: commands.some(cmd => cmd.operation === 'createScan' && cmd.library === 'lib1'
    && /^[0-9a-f]{32}$/.test(cmd.key) && cmd.body?.mode === 'incremental' && cmd.body?.require_complete === false && !cmd.replay),
  // The decision carried the rendered validator; the stale second one got 412 and a conflict message.
  if_match_conflict_surfaced: commands.filter(cmd => cmd.operation === 'decideMatch').map(cmd => cmd.status).join(',') === '200,412'
    && commands.some(cmd => cmd.operation === 'decideMatch' && cmd.if_match === '"r1"' && cmd.body?.decision === 'accept' && cmd.body?.candidate_id === 'mc1')
    && /changed somewhere else/.test(c.staleDecisionMessage ?? ''),
  demuxe_bytes_match_install_receipt: demuxeMismatches.length === 0,
  no_permission_granted_beyond_fullscreen: observed.permissionsGranted.every(p => p === 'fullscreen'),
};
const passed = Object.values(results).every(Boolean) && !c.exception;
const demuxePkg = JSON.parse(readFileSync(join(demuxe, 'package.json'), 'utf8'));
const demuxeInstall = existsSync(join(demuxe, 'playscale-package.json')) ? JSON.parse(readFileSync(join(demuxe, 'playscale-package.json'), 'utf8')) : {};
const receipt = {
  kind: 'topcoat-electron-local-origin-smoke-proof',
  scope: 'Limited Wave 0 smoke proof of local rendering/playback under the strict CSP. Not qualification of plan sections 12.2-12.3, 14.4 or 15.2-15.3: services are mocked, attachment/ownership/locking are not exercised.',
  wave: 0,
  passed,
  recorded_at: new Date().toISOString(),
  motion_commit: execFileSync('git', ['rev-parse', 'HEAD'], {cwd: root}).toString().trim(),
  worktree_dirty: execFileSync('git', ['status', '--porcelain', '--', '.', ':!qualification/desktop'], {cwd: root}).toString().trim().length > 0,
  environment: {electron: observed.electron, chromium: observed.chromium, node: observed.node, os: `${process.platform} ${release()}`, arch: observed.arch},
  topcoat: topcoatSource ? {version: topcoatSource[1], source: topcoatSource[2], features: 'router, view, tower, discover (no runtime)'} : null,
  proof_server_sha256: serverSha,
  proof_server_launch: {location: 'host-temporary-directory', sha256: launchServerSha, matches_build: launchServerSha === serverSha},
  rust_toolchain: '1.98.0',
  origin: 'single loopback origin serving Topcoat HTML, /ui assets, /assets/demuxe, mock /api/v2 and media',
  demuxe: {name: demuxePkg.name, version: demuxePkg.version, archive_sha256: demuxeInstall.archive_sha256 ?? null,
    files_verified: Object.keys(installReceipt.files ?? {}).length, mismatched_files: demuxeMismatches},
  // Pages are sent with Cache-Control: no-store, which Chromium excludes from the
  // back/forward cache; false means Back exercised a fresh load, not pageshow(persisted).
  bfcache_restored: c.afterBack?.restoredFromBfcache ?? null,
  media: {generator: 'ffmpeg lavfi testsrc2+sine, H.264 High/AAC-LC, 640x360@24, 12 s, faststart', sha256: mediaSha},
  results,
  observations: c,
  csp_console: observed.console.filter(line => /Content Security Policy|Refused/i.test(line)),
  permission_requests: observed.permissionRequests,
  permission_checks: [...new Set(observed.permissionChecks)],
  navigation_blocked: observed.navigationBlocked,
  commands,
  http: {requests: requests.length, deliveries_admitted: admitted, deliveries_closed: closed},
  limitations: [
    'Proof server uses a mock UiQueryFacade, mock session exchange and mock playback routes; it is not the Motion server.',
    'Bootstrap passed through the child-process environment of the launcher, not yet an inherited pipe.',
    'Native route of the installed reduced Demuxe package only; no Wasm/WebCodecs providers are installed.',
    'Audio muted; audible output, A/V sync, colour and HDR not observed. Hidden window, unpackaged, unsigned.',
    'Player bridge renews its active delivery lease; viewing authority, progress events and generation switching remain unimplemented.',
    'Back navigation reloaded the page (no-store HTML is not bfcache-eligible), so the pageshow(persisted) restart path was not exercised in Electron.',
  ],
};
const dir = join(root, 'qualification', 'desktop');
mkdirSync(dir, {recursive: true});
const file = join(dir, `topcoat-electron-proof-${process.platform}-${observed.arch}.json`);
writeFileSync(file, JSON.stringify(receipt, null, 2) + '\n');
console.log(`${passed ? 'PASSED' : 'FAILED'}: ${file}`);
console.log(JSON.stringify(results, null, 2));
process.exit(passed ? 0 : 1);

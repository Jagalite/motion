import {createDemuxePlayer} from '/demuxe.js';
import {DemuxeRuntime} from '/assets/demuxe/web/generated/index.js';

const assert = (value, message) => { if (!value) throw Error(message); };
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
async function advances(core) {
  const before = core.state.currentTime;
  for (let i = 0; i < 40; i++) { await wait(100); if (core.state.currentTime > before + .15) return; }
  throw Error('Native playback did not advance');
}
const mount = async element => {
  element.style.cssText = 'display:block;width:480px;height:270px';
  document.body.append(element);
  await element.ready;
  return element;
};
const remove = async element => { await element.destroy(); element.remove(); };

export async function runSessionTests() {
  const checks = [], elements = [];
  let synthetic;
  const compile = WebAssembly.compile;
  try {
    const failed = await Promise.allSettled([createDemuxePlayer(), createDemuxePlayer()]);
    assert(failed.every(result => result.status === 'rejected'), 'Initial failed manifest should reject both callers');
    const [a, b] = await Promise.all([createDemuxePlayer(), createDemuxePlayer()]);
    elements.push(a, b);
    assert(a.runtime === b.runtime, 'Concurrent creation must share one runtime');
    await Promise.all([mount(a), mount(b)]);
    let locked = false;
    try { a.runtime = undefined; } catch (error) { locked = error.code === 'INVALID_ARGUMENT'; }
    assert(locked && a.runtime === b.runtime, 'Connected runtime ownership must be fixed');
    await Promise.all([a.open('/fixture.mp4'), b.open('/fixture.mp4')]);
    await Promise.all([a.player.setMuted(true), b.player.setMuted(true)]);
    await Promise.all([a.player.play(), b.player.play()]);
    await Promise.all([advances(a.player), advances(b.player)]);
    checks.push('failed manifest retry and concurrent shared runtime initialization');
    checks.push('two real native players advance; runtime reassignment rejected');
    const runtime = a.runtime;
    await remove(a);
    await advances(b.player);
    const c = await createDemuxePlayer(); elements.push(c);
    assert(c.runtime === runtime, 'Replacement must reuse the surviving runtime');
    await mount(c);
    await c.open('/fixture.mp4');
    await c.player.setMuted(true); await c.player.play(); await advances(c.player);
    // Let thumbnail scheduling run: its provider acquisitions must also share
    // the manifest instead of silently creating independent runtimes.
    await wait(4500);
    checks.push('destroying and replacing a player preserves other playback and runtime');
    const failedPlayer = await createDemuxePlayer(); elements.push(failedPlayer); await mount(failedPlayer);
    let openFailed = false;
    try { await failedPlayer.open('/missing.mp4'); } catch { openFailed = true; }
    assert(openFailed, 'Missing source must fail');
    await remove(failedPlayer);
    const cancelled = await createDemuxePlayer(); elements.push(cancelled); await mount(cancelled);
    const pending = cancelled.open('/slow.mp4').then(() => false, () => true);
    await remove(cancelled);
    assert(await pending, 'Destroyed pending open must reject');
    assert(b.runtime === runtime, 'Failed and cancelled players must not replace shared runtime');
    await advances(b.player);
    checks.push('failed and cancelled opens leave the shared runtime and active playback usable');

    // A valid minimal Wasm module exercises fetch/compile caching, not codec execution.
    const config = await (await fetch('/test-config')).json();
    synthetic = new DemuxeRuntime({assetBase: '/assets/demuxe/', qualifiedProviders: config.qualified});
    await synthetic.providers.load('cache-fixture.json');
    let compilations = 0;
    WebAssembly.compile = async bytes => { compilations++; return compile(bytes); };
    const prepared = async () => {
      const element = document.createElement('demuxe-player');
      element.setAttribute('asset-base', '/assets/demuxe/'); element.runtime = synthetic;
      elements.push(element); await mount(element); return element;
    };
    const [x, y] = await Promise.all([prepared(), prepared()]);
    const reports = await Promise.all([x.player.prepare(['inspector']), y.player.prepare(['inspector'])]);
    assert(reports.every(report => report.assets[0].status === 'ready'), JSON.stringify(reports));
    assert(compilations === 1, 'Concurrent component preparation must compile once');
    await remove(x); await remove(y);
    const z = await prepared();
    const warm = await z.player.prepare(['inspector']);
    assert(warm.assets[0].status === 'ready' && compilations === 1, 'Replacement component must reuse compiled Wasm');
    const requests = await (await fetch('/requests')).json();
    assert(requests['/assets/demuxe/demuxe-providers.json'] === 2, 'One failed and one successful manifest fetch expected');
    assert(requests['/assets/demuxe/web/engine-remux/remux.wasm'] === 1, 'Wasm must download exactly once');
    checks.push('three component players share one Wasm download and one compilation across destruction');
    const result = {passed: true, checks, compilations, wasmRequests: requests['/assets/demuxe/web/engine-remux/remux.wasm'],
      manifestRequests: requests['/assets/demuxe/demuxe-providers.json'], cache: synthetic.cacheStats,
      isolation: crossOriginIsolated, userAgent: navigator.userAgent};
    document.body.insertAdjacentHTML('beforeend', '<pre id="result"></pre>');
    document.getElementById('result').textContent = JSON.stringify(result, null, 2);
    return result;
  } finally {
    WebAssembly.compile = compile;
    await Promise.allSettled(elements.map(remove));
    await synthetic?.destroy();
  }
}

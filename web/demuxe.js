// Share one application-owned runtime when supported by the installed package.
const assetBase = '/assets/demuxe/';
let session;

async function loadSession() {
  const [{DemuxeRuntime}, {definePlayerElement, DemuxePlayerElement}] = await Promise.all([
    import('/assets/demuxe/web/generated/index.js'),
    import('/assets/demuxe/web/generated/player/index.js'),
  ]);
  definePlayerElement();
  // npm 1.0.0 owns resources per player. Newer packages can lend an
  // application-owned runtime only when BOTH sides expose the public API.
  if (typeof DemuxeRuntime !== 'function' || !('runtime' in DemuxePlayerElement.prototype)) {
    return null;
  }
  const runtime = new DemuxeRuntime({assetBase});
  try {
    await runtime.providers.load('demuxe-providers.json');
    return runtime;
  } catch (error) {
    await runtime.destroy();
    throw error;
  }
}

export async function createDemuxePlayer() {
  // Share concurrent initialization; allow a later open to retry a failed load.
  session ??= loadSession().catch(error => { session = undefined; throw error; });
  const runtime = await session;
  const element = document.createElement('demuxe-player');
  if (runtime) element.runtime = runtime;
  element.setAttribute('asset-base', assetBase);
  element.setAttribute('controls', '');
  element.showSourceControls = false;
  element.allowFileDrop = false;
  return element;
}

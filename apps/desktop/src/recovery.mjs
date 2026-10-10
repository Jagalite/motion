// Restore transport records before any server page can acquire a new player.
// This temporary document has the selected origin but contains no server code.
import {randomBytes} from 'node:crypto';
import {scopedRecords} from './outbox.mjs';

export async function restoreViewing(view, ses, origin, history, store) {
  const scheme = new URL(origin).protocol.slice(0, -1);
  const restoreUrl = `${origin}/__motion_restore_${randomBytes(24).toString('hex')}`;
  ses.protocol.handle(scheme, request => new Response(
    request.url === restoreUrl ? '<!doctype html><title>Restoring viewing history</title>' : '',
    {status: request.url === restoreUrl ? 200 : 403, headers: {
      'Content-Type': 'text/html', 'Cache-Control': 'no-store',
      'Content-Security-Policy': "default-src 'none'; base-uri 'none'; frame-ancestors 'none'",
    }}));
  try {
    await view.webContents.loadURL(restoreUrl);
    const recovered = await view.webContents.executeJavaScript(`Object.fromEntries(Object.keys(localStorage).filter(key => key.startsWith('motion:viewing:')).map(key => [key, localStorage.getItem(key)]))`);
    const records = {...store.load(history.scope, history.principal), ...scopedRecords(recovered, history.principal)};
    // Save before replacing browser storage. Encryption failure preserves the
    // browser copy and prevents attachment. Exact retries remain server-fenced.
    store.save(history.scope, history.principal, records);
    await view.webContents.executeJavaScript(`localStorage.clear(); Object.entries(${JSON.stringify(records)}).forEach(([key, value]) => localStorage.setItem(key, value))`);
  } finally {
    ses.protocol.unhandle(scheme);
  }
}

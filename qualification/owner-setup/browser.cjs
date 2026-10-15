// Headless Chromium side of the first-run owner walkthrough (walkthrough.py).
//
//   node browser.cjs pair  BASE OUT   # pair from /signin, wait for approval, verify access
//   node browser.cjs check BASE OUT   # reuse the saved session after a server restart
//
// Requires NODE_PATH with playwright. BASE must be a literal loopback origin.
const {chromium} = require('playwright');
const fs = require('node:fs');
const path = require('node:path');

const [phase, base, out] = process.argv.slice(2);
const url = new URL(base);
if (url.origin !== base || !['127.0.0.1', '[::1]'].includes(url.hostname)) throw new Error('BASE must be a literal loopback origin');
fs.mkdirSync(out, {recursive: true});
const state = path.join(out, 'browser-state.json');
const write = (name, value) => fs.writeFileSync(path.join(out, name), JSON.stringify(value, null, 2));

(async () => {
  const browser = await chromium.launch();
  const result = {phase, errors: []};
  try {
    const context = await browser.newContext(phase === 'check' ? {storageState: state} : {});
    const page = await context.newPage();
    page.on('pageerror', e => result.errors.push(e.message));
    const me = () => page.evaluate(async () => {
      const r = await fetch('/api/v2/me');
      return {status: r.status, body: r.ok ? await r.json() : null};
    });
    if (phase === 'pair') {
      await page.goto(base + '/signin');
      result.signin_first_run_hint = await page.getByText('New server with no users?', {exact: false}).isVisible();
      await page.getByRole('button', {name: 'Pair this browser'}).click();
      const message = page.getByText('Approve this code from an administrator device:', {exact: false});
      await message.waitFor({timeout: 15000});
      result.code = (await message.innerText()).split(':').pop().trim();
      fs.writeFileSync(path.join(out, 'code.txt'), result.code);
      await page.screenshot({path: path.join(out, 'browser-code.png')});
      result.before = (await me()).status;
      // The bridge polls the claim and reloads once the browser is approved.
      for (let i = 0; i < 600; i++) {
        await page.waitForTimeout(500);
        const now = await me().catch(() => ({status: 0}));
        if (now.status === 200) { result.me = now.body; break; }
      }
      if (!result.me) throw new Error('the browser was not approved in time');
      await context.storageState({path: state});
    } else {
      await page.goto(base + '/');
      const now = await me();
      result.status = now.status;
      result.me = now.body;
    }
    await page.goto(base + '/settings/access');
    await page.getByText('Access settings loaded.', {exact: false}).waitFor({timeout: 20000});
    result.settings_access = true;
    result.owner_setup_offered = await page.getByText('First run: create the Owner').count();
    await page.screenshot({path: path.join(out, `browser-${phase}-access.png`), fullPage: false});
  } catch (error) {
    result.failure = String(error.stack || error);
  } finally {
    write(`browser-${phase}.json`, result);
    await browser.close();
  }
  process.exit(result.failure ? 1 : 0);
})();

// Headless Chromium side of the first-run owner walkthrough (walkthrough.py).
//
//   node browser.cjs pair    BASE OUT          # pair from /signin, wait for approval, verify access
//   node browser.cjs check   BASE OUT          # reuse the saved session after a server restart
//   node browser.cjs change  BASE OUT          # the Owner browser creates user Sam; the change
//                                              # is approved in the TUI (Action approvals)
//   node browser.cjs desktop BASE OUT CRED     # an unassigned administrator browser (as the
//                                              # desktop app's) creates the Owner and approves a
//                                              # pairing with deliberate administration
//
// Approval handoff: the browser writes OUT/approval-N.txt with the code it
// shows and waits for OUT/approved-N, written by walkthrough.py after the TUI
// approved it. Requires NODE_PATH with playwright and axe-core.
const {chromium} = require('playwright');
const fs = require('node:fs');
const path = require('node:path');

const [phase, base, out, credential] = process.argv.slice(2);
const url = new URL(base);
if (url.origin !== base || !['127.0.0.1', '[::1]'].includes(url.hostname)) throw new Error('BASE must be a literal loopback origin');
fs.mkdirSync(out, {recursive: true});
const state = path.join(out, 'browser-state.json');
const write = (name, value) => fs.writeFileSync(path.join(out, name), JSON.stringify(value, null, 2));
const sleep = ms => new Promise(r => setTimeout(r, ms));
let approvals = 0;

async function audit(page, result, name) {
  await page.evaluate(fs.readFileSync(require.resolve('axe-core/axe.min.js'), 'utf8'));
  const violations = await page.evaluate(async () => {
    const r = await axe.run(document, {runOnly: {type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa']}});
    return r.violations.map(v => ({id: v.id, impact: v.impact, nodes: v.nodes.map(n => n.target)}));
  });
  const geometry = await page.evaluate(() => ({viewport: innerWidth, scroll: document.documentElement.scrollWidth}));
  await page.screenshot({path: path.join(out, `audit-${name}.png`), fullPage: true});
  result.audits.push({name, violations, overflow: geometry.scroll > geometry.viewport});
}

// Complete the action-approval flow inside scope via the TUI operator.
async function approveInTui(scope) {
  const n = ++approvals;
  await scope.getByRole('button', {name: 'Request approval for this change'}).click();
  const detail = scope.locator('p.access-detail', {hasText: 'Approval code:'}).last();
  await detail.waitFor({timeout: 15000});
  fs.writeFileSync(path.join(out, `approval-${n}.txt`), (await detail.innerText()).split(':').pop().trim());
  for (let i = 0; i < 600 && !fs.existsSync(path.join(out, `approved-${n}`)); i++) await sleep(250);
  await scope.getByRole('button', {name: 'Check approval'}).click();
  await scope.getByRole('button', {name: 'Submit the reviewed change'}).click();
}
async function openAccess(page) {
  await page.goto(base + '/settings/access');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor({timeout: 20000});
}
const fetchJson = (page, route) => page.evaluate(async route => {
  const r = await fetch(route);
  return {status: r.status, body: r.ok ? await r.json() : null};
}, route);

(async () => {
  const browser = await chromium.launch();
  const result = {phase, errors: [], audits: []};
  try {
    const context = await browser.newContext(['check', 'change'].includes(phase) ? {storageState: state, viewport: {width: 1280, height: 900}} : {viewport: {width: 1280, height: 900}});
    const page = await context.newPage();
    page.on('pageerror', e => result.errors.push(e.message));
    const me = () => fetchJson(page, '/api/v2/me');
    if (phase === 'pair') {
      await page.goto(base + '/signin');
      result.signin_first_run_hint = await page.getByText('New server with no users?', {exact: false}).isVisible();
      await audit(page, result, 'signin');
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
      await openAccess(page);
      result.settings_access = true;
      result.owner_setup_offered = await page.getByText('First run: create the Owner').count();
    } else if (phase === 'check') {
      await page.goto(base + '/');
      const now = await me();
      result.status = now.status;
      result.me = now.body;
      await openAccess(page);
      result.settings_access = true;
    } else if (phase === 'change') {
      // A paired-only Owner browser is not freshly authenticated: its admin
      // change needs another principal's approval, here the TUI operator.
      await openAccess(page);
      const users = page.locator('#users');
      await users.getByText('Create a local user', {exact: true}).click();
      const create = users.locator('details', {hasText: 'Create a local user'}).last();
      await create.getByLabel('User name').fill('Sam');
      await create.getByRole('button', {name: 'Create user'}).click();
      await approveInTui(create);
      for (let i = 0; i < 40 && !result.created; i++) {
        const list = await fetchJson(page, '/api/v2/users?limit=50');
        result.created = (list.body?.items ?? []).some(u => u.name === 'Sam');
        if (!result.created) await sleep(250);
      }
    } else if (phase === 'desktop') {
      const exchange = await page.goto(base + '/signin').then(() => page.evaluate(async credential => {
        const r = await fetch('/api/v2/auth/session', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({kind: 'credential', credential})});
        return r.status;
      }, credential));
      result.session = exchange;
      await openAccess(page);
      result.owner_setup_offered = await page.getByText('First run: create the Owner').count();
      await audit(page, result, 'access-first-run');
      const setup = page.locator('article', {hasText: 'First run: create the Owner'});
      await setup.getByRole('button', {name: 'Create the Owner'}).click();
      await approveInTui(setup);
      await page.waitForLoadState('load');
      const after = await fetchJson(page, '/api/v2/access/setup');
      result.owner = after.body?.owner ?? null;
      // A second device asks to pair; approve it here for the preselected Owner.
      for (let i = 0; i < 600 && !fs.existsSync(path.join(out, 'second-pairing.json')); i++) await sleep(250);
      const pairing = JSON.parse(fs.readFileSync(path.join(out, 'second-pairing.json'), 'utf8'));
      await openAccess(page);
      const card = page.locator('article', {hasText: pairing.user_code}).first();
      await card.waitFor();
      result.preselected = await card.locator('select').first().inputValue();
      await audit(page, result, 'access-pairing-preselected');
      await card.getByLabel('Enter the code shown on the requesting device').fill(pairing.user_code);
      await card.getByLabel('Administer this server', {exact: true}).check();
      await card.getByRole('button', {name: 'Approve this device'}).click();
      result.admin_refused_without_confirmation = await card.getByText('Administration is selected.', {exact: false}).isVisible({timeout: 5000}).catch(() => false);
      await card.getByLabel('If Administer this server is selected: I deliberately grant administration to this device.').check();
      await card.getByRole('button', {name: 'Approve this device'}).click();
      await approveInTui(card);
      for (let i = 0; i < 40 && !result.second_status; i++) {
        const list = await fetchJson(page, '/api/v2/auth/pairings?limit=50');
        const p = (list.body?.items ?? []).find(item => item.id === pairing.id);
        if (p && p.status !== 'pending') result.second_status = p.status;
        else await sleep(250);
      }
    }
  } catch (error) {
    result.failure = String(error.stack || error);
  } finally {
    write(`browser-${phase}.json`, result);
    await browser.close();
  }
  process.exit(result.failure ? 1 : 0);
})();

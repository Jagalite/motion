// Isolated real-client qualification of email delivery and registration.
//
// Drives a running Motion server (isolated data directory, loopback origin)
// with headless Chromium. Mail goes to qualification/auth-signin/mail.py on
// loopback: first through the SMTP relay mode, then through native direct
// delivery's explicit loopback qualification route (DKIM-signed, verified by
// tests/fixtures/mail/dkim_verify.mjs). No real mailbox or DNS is used.
//
//   MOTION_BASE=http://127.0.0.1:8960 MOTION_DATA=/path/data MAILBOX=/path/mail \
//   SINK_PORT=2525 OUT=/path/out NODE_PATH=/path/with/playwright node browser.cjs
const {chromium} = require('playwright');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const {execFileSync} = require('node:child_process');

const base = process.env.MOTION_BASE;
const operator = fs.readFileSync(path.join(process.env.MOTION_DATA, 'admin-token'), 'utf8').trim();
const mailbox = process.env.MAILBOX;
const sinkPort = Number(process.env.SINK_PORT);
const out = process.env.OUT;
fs.mkdirSync(out, {recursive: true});
const report = {started: new Date().toISOString(), steps: [], audits: [], consoleErrors: [], limits: [
  'Loopback mail sink only; no real mailbox, provider, DNS or port-25 delivery.',
  'Chromium only; fixed CSS viewports do not emulate touch hardware or screen readers.',
]};
const step = (name, detail = {}) => { report.steps.push({name, at: new Date().toISOString(), ...detail}); save(); console.log('step:', name); };
const save = () => fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify(report, null, 2));

async function api(method, route, body, extra = {}) {
  const headers = {origin: base, accept: 'application/json', ...extra.headers};
  if (extra.operator !== false) headers.authorization = `Bearer ${extra.token ?? operator}`;
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (method !== 'GET') headers['idempotency-key'] = crypto.randomUUID();
  if (extra.revision) headers['if-match'] = `"r-${extra.revision}"`;
  const response = await fetch(base + route, {method, headers, body: body === undefined ? undefined : JSON.stringify(body)});
  const text = await response.text();
  const data = text ? JSON.parse(text) : null;
  if (!response.ok && !extra.allow) throw new Error(`${method} ${route}: ${response.status} ${text}`);
  return {status: response.status, data, headers: response.headers};
}
const messages = () => fs.readdirSync(mailbox).filter(f => f.endsWith('.eml')).map(f => path.join(mailbox, f))
  .sort((a, b) => fs.statSync(a).mtimeMs - fs.statSync(b).mtimeMs);
async function nextMessage(before, match = () => true) {
  for (let i = 0; i < 300; i++) {
    const fresh = messages().slice(before).map(file => ({file, text: fs.readFileSync(file, 'latin1')})).filter(m => match(m.text));
    if (fresh.length) return fresh[fresh.length - 1];
    await new Promise(r => setTimeout(r, 100));
  }
  throw new Error('no message delivered to the isolated mailbox');
}
const codeIn = text => (text.match(/(?:code is |Test code: )(\d{8})/) ?? [])[1];

async function audit(page, name, width) {
  await page.setViewportSize({width, height: 1000});
  await page.screenshot({path: path.join(out, `${name}-${width}.png`), fullPage: false});
  await page.addScriptTag({path: require.resolve('axe-core/axe.min.js')});
  const axe = await page.evaluate(async () => {
    const r = await axe.run(document, {runOnly: {type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa']}});
    return r.violations.map(v => ({id: v.id, impact: v.impact, nodes: v.nodes.map(n => n.target)}));
  });
  const geometry = await page.evaluate(() => ({viewport: innerWidth, scroll: document.documentElement.scrollWidth}));
  report.audits.push({name, width, axe, geometry}); save();
  assert.equal(geometry.scroll, width, `${name} ${width}: horizontal overflow`);
  assert.deepEqual(axe, [], `${name} ${width}: accessibility violations`);
}

// Sensitive admin changes from a paired browser need action-bound approval
// from another authorized principal: here the local operator approves the
// displayed code, then the browser explicitly submits the reviewed change.
async function approveAndSubmit(page, scope, action) {
  await scope.getByRole('button', {name: 'Request approval for this change'}).click();
  const detail = scope.getByText('Approval code:', {exact: false}).last();
  await detail.waitFor();
  const code = (await detail.innerText()).split(':').pop().trim();
  const list = await api('GET', '/api/v2/admin/auth/fresh-approvals?limit=200');
  const proof = list.data.items.filter(p => p.status === 'pending' && p.action === action).sort((a, b) => b.created_at - a.created_at)[0];
  assert.ok(proof, `pending ${action} approval`);
  await api('POST', `/api/v2/auth/proofs/${proof.id}/approve`, {code});
  await scope.getByRole('button', {name: 'Check approval'}).click();
  await scope.getByRole('button', {name: 'Submit the reviewed change'}).click();
}

async function bootstrap() {
  await api('PUT', '/api/v2/admin/auth/settings', {public_origin: base}, {revision: '1'});
  const profiles = (await api('GET', '/api/v2/profiles?limit=200')).data.items.map(p => p.id);
  const grant = {profile_ids: profiles, permissions: ['catalog:read', 'events:read', 'system:admin'],
    policy: {library_ids: [], allow_unrated: true, allowed_ratings: [], deny_rated: false, blocked_labels: []}};
  const admin = (await api('POST', '/api/v2/users', {name: 'Qualification administrator', disabled: false, ...grant})).data;
  const pairing = (await api('POST', '/api/v2/auth/pairings', {device_name: 'Qualification browser', client_name: 'chromium'}, {operator: false})).data;
  await api('POST', `/api/v2/auth/pairings/${pairing.id}/approve`, {user_code: pairing.user_code, user_id: admin.id, user_revision: admin.revision, ...grant});
  const claim = (await api('POST', `/api/v2/auth/pairings/${pairing.id}/claim`, {device_code: pairing.device_code}, {operator: false})).data;
  const relay = {enabled: false, mode: 'relay', host: '127.0.0.1', port: sinkPort, username: '', from: 'Motion <motion@example.test>', loopback_test: true, password: {action: 'retain'}};
  const draft = (await api('PUT', '/api/v2/admin/auth/email', relay, {revision: '1'})).data;
  await api('POST', '/api/v2/admin/auth/email/test', {address: 'operator@example.test'});
  await api('PUT', '/api/v2/admin/auth/email', {...relay, enabled: true}, {revision: draft.revision});
  const member = (await api('POST', '/api/v2/users', {name: 'Approved member', disabled: false, profile_ids: profiles, permissions: ['catalog:read'], policy: grant.policy})).data;
  const invitee = (await api('POST', '/api/v2/users', {name: 'Invited member', disabled: false, profile_ids: profiles, permissions: ['catalog:read', 'events:read'], policy: grant.policy})).data;
  step('bootstrap', {admin: admin.id, member: member.id, invitee: invitee.id});
  return {token: claim.access_token, profiles};
}

async function registerInBrowser(page, address, name, expected) {
  await page.goto(base + '/signin');
  const area = page.locator('[data-registration]');
  await area.waitFor();
  const before = messages().length;
  await area.getByLabel('Email address', {exact: true}).fill(address);
  if (name) await area.getByLabel('Your name (shown to the administrator)').fill(name);
  await area.getByRole('button', {name: 'Send verification code'}).click();
  await area.getByLabel('Verification code').waitFor();
  const message = await nextMessage(before, t => t.includes(`<${address}>`) && t.includes('verification code'));
  await area.getByLabel('Verification code').fill(codeIn(message.text));
  await area.getByRole('button', {name: 'Verify email'}).click();
  const outcome = area.locator('[data-registration-outcome]');
  await outcome.waitFor();
  assert.equal(await outcome.getAttribute('data-registration-outcome'), expected);
  return message;
}

async function setPolicy(page, policy) {
  await page.goto(base + '/settings/access#registration');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  const scope = page.locator('#registration');
  await scope.locator('summary').first().click();
  await scope.getByLabel('Registration policy').selectOption(policy);
  if (policy === 'open') await scope.getByRole('checkbox', {name: 'Browse catalog'}).check();
  await scope.getByRole('button', {name: 'Save registration policy'}).click();
  await approveAndSubmit(page, scope, 'registration.write');
  await page.waitForURL(/settings\/access/);
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  const current = (await api('GET', '/api/v2/admin/auth/registration')).data;
  assert.equal(current.policy, policy);
}

(async () => {
  const {token} = await bootstrap();
  const browser = await chromium.launch({headless: true});
  report.browser = browser.version();
  report.playwright = require('playwright/package.json').version;
  const admin = await browser.newContext({viewport: {width: 1440, height: 1000}});
  const page = await admin.newPage();
  page.on('pageerror', e => report.consoleErrors.push(e.message));
  await page.goto(base + '/signin');
  await page.evaluate(async credential => {
    const r = await fetch('/api/v2/auth/session', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({kind: 'credential', credential})});
    if (!r.ok) throw new Error(`session ${r.status}`);
  }, token);
  await page.goto(base + '/settings/access');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  step('admin browser session from paired credential');

  // Closed by default: no public registration offered.
  const anonymous = await browser.newContext({viewport: {width: 1440, height: 1000}});
  const visitor = await anonymous.newPage();
  visitor.on('pageerror', e => report.consoleErrors.push(e.message));
  await visitor.goto(base + '/signin');
  await visitor.getByRole('button', {name: 'Send sign-in code'}).waitFor();
  assert.equal(await visitor.locator('[data-registration]').count(), 0);
  step('closed policy offers no registration');

  // Open registration over the SMTP relay.
  await setPolicy(page, 'open');
  await registerInBrowser(visitor, 'open-relay@example.test', 'Open relay member', 'account_active');
  await visitor.getByRole('link', {name: 'Continue to Motion'}).click();
  const me = await visitor.evaluate(async () => (await fetch('/api/v2/me')).status);
  assert.equal(me, 200);
  step('open registration via relay activates usable access');
  for (const width of [1440, 390, 320]) {
    await visitor.goto(base + '/signin');
    await visitor.locator('[data-registration]').waitFor();
    await audit(visitor, 'signin-registration-open', width);
  }

  // Switch to native direct delivery through the explicit loopback route.
  await page.goto(base + '/settings/access#email');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  let email = page.locator('#email');
  await email.getByText('Configure email delivery').click();
  await email.getByLabel('Delivery mode').selectOption('direct');
  await email.getByLabel('Sending hostname (EHLO name; should match reverse DNS)').fill('mail.example.test');
  await email.getByLabel('Envelope sender for bounces (an external mailbox you read)').fill('bounces@example.test');
  await email.getByLabel('DKIM selector').fill('motion1');
  await email.getByLabel('DKIM key action').selectOption('generate');
  await email.getByLabel('Isolated loopback qualification port (testing only; plaintext to 127.0.0.1)').fill(String(sinkPort));
  await email.getByLabel('Enable email sign-in').uncheck();
  await email.getByLabel(/If disabling email/).check();
  await email.getByRole('button', {name: 'Save email settings'}).click();
  await approveAndSubmit(page, email, 'email.write');
  email = page.locator('#email');
  await email.getByText('Native direct delivery', {exact: false}).first().waitFor();
  const settings = (await api('GET', '/api/v2/admin/auth/email')).data;
  assert.equal(settings.mode, 'direct');
  assert.ok(settings.dkim.record.startsWith('v=DKIM1; k=rsa; p='));
  await email.getByText('DNS records to publish').waitFor();
  step('direct delivery configured with generated DKIM key', {record_name: settings.dkim.record_name, key_id: settings.dkim.key_id});

  let before = messages().length;
  await email.getByLabel('Explicit test email destination').fill('receipt@example.test');
  await email.getByRole('button', {name: 'Send test email'}).click();
  await approveAndSubmit(page, email.locator('article', {hasText: 'Send a test message'}).first(), 'email.test');
  const test = await nextMessage(before, t => t.includes('Motion delivery test'));
  const verified = JSON.parse(execFileSync('node', [path.join(__dirname, '../../tests/fixtures/mail/dkim_verify.mjs'), test.file, settings.dkim.record]));
  assert.equal(verified.ok, true, JSON.stringify(verified));
  await email.getByLabel('Test code from the received message').fill(codeIn(test.text));
  await email.getByRole('button', {name: 'Confirm receipt'}).click();
  await email.getByText('Code entered', {exact: false}).waitFor();
  step('direct test accepted, DKIM independently verified, receipt confirmed in UI', {dkim: verified});

  await email.getByText('Configure email delivery').click();
  await email.getByLabel('Enable email sign-in').check();
  await email.getByRole('button', {name: 'Save email settings'}).click();
  await approveAndSubmit(page, email, 'email.write');
  await page.locator('#email').getByText('Enabled · Native direct delivery').waitFor();
  step('direct delivery enabled after a successful test');

  // Approval required, over direct delivery.
  await setPolicy(page, 'approval_required');
  const approval = await registerInBrowser(visitor, 'requester@example.test', 'Requesting member', 'pending_approval');
  assert.match(JSON.parse(execFileSync('node', [path.join(__dirname, '../../tests/fixtures/mail/dkim_verify.mjs'), approval.file, settings.dkim.record])).reason, /verified/);
  await page.goto(base + '/settings/access#requests');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  const request = page.locator('#requests article', {hasText: 'requester@example.test'}).first();
  await request.getByLabel('Local user receiving this identity').selectOption({label: 'Approved member'});
  await request.getByLabel('I authorize this verified email address to sign in as the selected local user.').check();
  await request.getByRole('button', {name: 'Approve verified identity'}).click();
  await approveAndSubmit(page, request, 'request.approve');
  await page.waitForURL(/settings\/access/);
  const approvedContext = await browser.newContext();
  const approved = await approvedContext.newPage();
  await approved.goto(base + '/signin');
  before = messages().length;
  await approved.getByLabel('Email address', {exact: true}).first().fill('requester@example.test');
  await approved.getByRole('button', {name: 'Send sign-in code'}).click();
  const login = await nextMessage(before, t => t.includes('<requester@example.test>') && t.includes('sign-in code'));
  await approved.getByLabel('One-time email code').fill(codeIn(login.text));
  await approved.getByRole('button', {name: 'Verify code'}).click();
  await approved.waitForURL(base + '/');
  assert.equal(await approved.evaluate(async () => (await fetch('/api/v2/me')).status), 200);
  step('approval-required registration pending until UI approval, then email login works');

  // Invite-only: an emailed invitation completes on registration.
  await setPolicy(page, 'invite_only');
  await page.goto(base + '/settings/access#enrollment');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  const invitations = page.locator('#enrollment');
  await invitations.getByText('Create an invitation').click();
  const create = invitations.locator('details', {hasText: 'Create an invitation'});
  await create.getByLabel('Invitation owner').selectOption({label: 'Invited member'});
  await create.getByLabel('Email the invitation to (optional; they register with this address)').fill('invitee@example.test');
  await create.getByRole('checkbox', {name: 'Browse catalog'}).check();
  before = messages().length;
  await create.getByRole('button', {name: 'Generate single-use invitation'}).click();
  await approveAndSubmit(page, create, 'invitation.create');
  const invite = await nextMessage(before, t => t.includes('You are invited to Motion'));
  assert.equal(codeIn(invite.text), undefined);
  await registerInBrowser(visitor, 'invitee@example.test', null, 'invitation_accepted');
  const listed = (await api('GET', '/api/v2/auth/invitations?limit=200')).data.items.find(i => i.email === 'invitee@example.test');
  assert.equal(listed.status, 'registered');
  step('invite-only registration completes the emailed invitation', {invitation: listed.id});

  // Activity and the new admin sections render accessibly.
  await page.goto(base + '/settings/access#email');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  const activity = await page.locator('[data-delivery-status]').count();
  assert.ok(activity >= 5, 'delivery activity lists messages');
  assert.equal(await page.locator('#email').getByText('@example.test').count() > 0, true, 'sender settings show addresses only in configuration');
  for (const id of ['email', 'registration']) {
    await page.locator('#' + id).evaluate(e => e.scrollIntoView({block: 'start'}));
    for (const width of [1440, 390, 320]) await audit(page, `admin-${id}`, width);
  }
  step('delivery activity and admin sections audited', {activity});

  // Back to relay for a final open registration (both transports serve registration).
  await setPolicy(page, 'open');
  await page.goto(base + '/settings/access#email');
  await page.getByText('Access settings loaded.', {exact: false}).waitFor();
  email = page.locator('#email');
  await email.getByText('Configure email delivery').click();
  await email.getByLabel('Delivery mode').selectOption('relay');
  await email.getByLabel('Mail server hostname').fill('127.0.0.1');
  await email.getByLabel('Port').fill(String(sinkPort));
  await email.getByLabel('Use an isolated local mail sink (loopback only)').check();
  await email.getByLabel('Enable email sign-in').uncheck();
  await email.getByLabel(/If disabling email/).check();
  await email.getByRole('button', {name: 'Save email settings'}).click();
  await approveAndSubmit(page, email, 'email.write');
  await page.locator('#email').getByText('Disabled · SMTP relay').waitFor();
  step('relay mode restored from the UI (disabled draft)');

  assert.deepEqual(report.consoleErrors, []);
  report.status = 'passed';
  report.finished = new Date().toISOString();
  save();
  await browser.close();
  console.log(JSON.stringify({status: report.status, steps: report.steps.length, audits: report.audits.length}));
})().catch(error => {
  report.status = 'failed';
  report.error = error.stack;
  save();
  console.error(error);
  process.exit(1);
});

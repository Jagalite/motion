import assert from 'node:assert/strict';
import {webcrypto} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {runInNewContext} from 'node:vm';

const source = readFileSync(new URL('./auth.js', import.meta.url), 'utf8');
class Element {
  constructor(tag) {
    this.tagName = tag;
    this.children = [];
    this.attributes = {};
    this.events = {};
    this.textContent = '';
    this.value = '';
    this.checked = false;
    this.disabled = false;
    this.hidden = false;
  }
  append(...children) { for (const child of children) { child.parent = this; this.children.push(child); } }
  setAttribute(key, value) { this.attributes[key] = String(value); }
  addEventListener(event, run) { this.events[event] = run; }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); }
  focus() { this.focused = true; }
  async click() { return await this.events.click?.({preventDefault() {}}); }
  *descendants() { yield this; for (const child of this.children) yield* child.descendants(); }
  byText(text) { return [...this.descendants()].find(child => child.textContent === text); }
  ofTag(tag) { return [...this.descendants()].filter(child => child.tagName === tag); }
}
function harness(fetch = async () => ({ok: true, status: 204}), extra = {}) {
  const document = {createElement: tag => new Element(tag), querySelector: selector => selector === 'meta[name="motion-csrf"]' ? {content: 'csrf-token'} : null};
  const location = {origin: 'https://motion.test', search: '', reload() {}, assign() {}};
  return runInNewContext(`${source}\n({api, mutation, secretUpdate, secretField, safeRedirect, errorMessage, revision, node, proofFlow, selectionRows, grantFields, confirmChange, signInPage, selfPage, pending, emailFlow, registrationFlow, emailSection, registrationSection, enrollmentSection, usersSection, inheritedGrant})`, {
    document, location, fetch, crypto: webcrypto, URL, URLSearchParams, TextEncoder,
    navigator: {}, ...extra,
  });
}
const response = (status, data) => ({ok: status >= 200 && status < 300, status, json: async () => data});
const plain = value => JSON.parse(JSON.stringify(value));

test('last administrator protection explains recovery rather than suggesting stale-data retry', () => {
  const client = harness();
  assert.match(client.errorMessage({status: 409, code: 'last_administrator'}), /another usable administrator/);
  assert.match(client.errorMessage({status: 412, code: 'revision_conflict'}), /Refresh this section/);
});

test('recovery safeguards do not offer stale-state retries', async () => {
  for (const code of ['last_administrator', 'last_signin_method']) {
    let calls = 0;
    const client = harness(async () => { calls++; return response(409, {code}); });
    const parent = new Element('div');
    await assert.rejects(client.mutation(parent, 'DELETE', '/auth/identities/i1', undefined, {revision: '1'}));
    assert.equal(calls, 1);
    assert.equal(parent.byText('Review the latest state'), undefined);
    assert.equal(parent.byText('Refresh and review'), undefined);
    assert.equal(client.pending.size, 0);
  }
});

test('writes use same-origin credentials, CSRF, explicit revision/proof and reject redirects', async () => {
  const calls = [];
  const client = harness(async (path, options) => { calls.push({path, options}); return response(200, {ok: true}); });
  await client.api('PUT', '/admin/auth/providers/p1', {client_secret: {action: 'retain'}}, {revision: '14', proof: 'proof-1', key: 'retry-key'});
  const {path, options} = calls[0];
  assert.equal(path, '/api/v2/admin/auth/providers/p1');
  assert.equal(options.credentials, 'same-origin');
  assert.equal(options.redirect, 'error');
  assert.equal(options.cache, 'no-store');
  assert.equal(options.headers['If-Match'], '"r-14"');
  assert.equal(options.headers['X-CSRF-Token'], 'csrf-token');
  assert.equal(options.headers['X-Motion-Action-Proof'], 'proof-1');
  assert.equal(options.headers['Idempotency-Key'], 'retry-key');
  assert.deepEqual(JSON.parse(options.body), {client_secret: {action: 'retain'}});
  await assert.rejects(client.api('POST', '/../../outside', {}), /Invalid API destination/);
  assert.equal(calls.length, 1);
});

test('GET requests never attach action proofs or write headers', async () => {
  let headers;
  const client = harness(async (_, options) => { headers = options.headers; return response(200, {}); });
  await client.api('GET', '/auth/methods', undefined, {proof: 'do-not-leak', key: 'not-used', revision: '2'});
  assert.deepEqual(plain(headers), {Accept: 'application/json'});
});

test('retain and clear never submit any secret, even a stale field value', () => {
  const client = harness();
  assert.deepEqual(plain(client.secretUpdate('retain', 'previous-value')), {action: 'retain'});
  assert.deepEqual(plain(client.secretUpdate('clear', 'previous-value')), {action: 'clear'});
  assert.deepEqual(plain(client.secretUpdate('replace', 'new-secret')), {action: 'replace', value: 'new-secret'});
  assert.throws(() => client.secretUpdate('replace', ''), /replacement secret/);
  assert.throws(() => client.secretUpdate('unexpected', 'bad'), /Choose how/);
});

test('secret replacement controls start empty and clear when retain or clear is selected', () => {
  const client = harness();
  const parent = new Element('div');
  const control = client.secretField(parent, 'Client secret', true);
  const input = parent.ofTag('input')[0];
  const action = parent.ofTag('select')[0];
  assert.equal(input.value, '');
  assert.equal(input.disabled, true);
  assert.equal(input.attributes.type, 'password');
  action.value = 'replace'; action.events.change();
  input.value = 'secret-in-memory';
  assert.equal(input.disabled, false);
  action.value = 'retain'; action.events.change();
  assert.equal(input.value, '');
  assert.deepEqual(plain(control.read()), {action: 'retain'});
});

test('provider text is rendered as text, never inserted as markup', () => {
  const client = harness();
  const value = '<img src=x onerror=alert(1)>';
  const element = client.node('h3', value);
  assert.equal(element.textContent, value);
  assert.equal(element.children.length, 0);
  assert.equal(element.innerHTML, undefined);
  assert.doesNotMatch(source, /\.innerHTML\s*=|insertAdjacentHTML|document\.write\(/);
});

test('redirects accept server-owned paths and HTTPS providers, reject other origins and unsafe schemes', () => {
  const client = harness();
  assert.equal(client.safeRedirect('/settings/signin'), 'https://motion.test/settings/signin');
  assert.equal(client.safeRedirect('https://idp.example/authorize?state=bound', true), 'https://idp.example/authorize?state=bound');
  assert.equal(client.safeRedirect('http://127.0.0.1:9090/authorize', true), 'http://127.0.0.1:9090/authorize');
  for (const value of ['javascript:alert(1)', 'data:text/html,bad', 'https://attacker.test/', '//attacker.test/', 'https://user:password@motion.test/']) {
    assert.throws(() => client.safeRedirect(value), /unsafe redirect/);
  }
  assert.throws(() => client.safeRedirect('http://public-idp.example/authorize', true), /unsafe redirect/);
});

test('uncertain mutation retries reuse one key without storing secrets in map keys or browser storage', async () => {
  const calls = [];
  const client = harness(async (_, options) => { calls.push(options); return response(503, {code: 'unavailable'}); }, {
    localStorage: {setItem() { throw new Error('Must not persist authentication data'); }},
    sessionStorage: {setItem() { throw new Error('Must not persist authentication data'); }},
  });
  const body = {client_secret: {action: 'replace', value: 'private-client-secret'}};
  const parent = new Element('div');
  await assert.rejects(client.mutation(parent, 'PUT', '/admin/auth/providers/p1', body, {revision: '1'}));
  await assert.rejects(client.mutation(parent, 'PUT', '/admin/auth/providers/p1', body, {revision: '1'}));
  assert.equal(calls[0].headers['Idempotency-Key'], calls[1].headers['Idempotency-Key']);
  const keys = [...client.pending.keys()];
  assert.equal(keys.length, 1);
  assert.match(keys[0], /^[a-f0-9]{64}$/);
  assert.ok(!keys[0].includes('private-client-secret'));
});

test('revision conflicts never silently replay and invalidate the old retry key', async () => {
  const calls = [];
  const client = harness(async (_, options) => { calls.push(options); return response(412, {code: 'revision_conflict'}); });
  const parent = new Element('div');
  for (let attempt = 0; attempt < 2; attempt++) await assert.rejects(client.mutation(parent, 'PUT', '/users/u1', {name: 'Someone'}, {revision: '3'}));
  assert.equal(calls.length, 2);
  assert.notEqual(calls[0].headers['Idempotency-Key'], calls[1].headers['Idempotency-Key']);
  assert.equal(client.pending.size, 0);
  assert.ok(parent.byText('Refresh and review'));
});

test('fresh approval requires explicit request, check, and reviewed resubmission', async () => {
  const calls = [];
  let committed = false;
  const client = harness(async (path, options) => {
    calls.push({path, options});
    if (path === '/api/v2/auth/proofs') return response(201, {id: 'proof1', code: '12345678', expires_at: '2099-01-01T00:00:00Z', status: 'pending'});
    if (path === '/api/v2/auth/proofs/proof1') return response(200, {status: 'approved'});
    if (options.headers['X-Motion-Action-Proof'] === 'proof1') { committed = true; return response(204); }
    return response(428, {code: 'recent_auth_required'});
  });
  const parent = new Element('div');
  await assert.rejects(client.mutation(parent, 'PUT', '/admin/auth/providers/p1', {enabled: false}, {revision: '2', action: 'provider.write', target: 'p1'}));
  assert.equal(calls.length, 1);
  assert.equal(committed, false);
  await parent.byText('Request approval for this change').click();
  assert.equal(calls.length, 2);
  assert.deepEqual(JSON.parse(calls[1].options.body), {action: 'provider.write', target: 'p1'});
  await parent.byText('Check approval').click();
  assert.equal(committed, false);
  assert.equal(calls.length, 3);
  await parent.byText('Submit the reviewed change').click();
  assert.equal(committed, true);
  assert.equal(calls.length, 4);
  assert.equal(calls[3].options.headers['If-Match'], '"r-2"');
});

test('denied or expired approval never exposes a commit button', async () => {
  for (const phase of ['pending', 'denied', 'expired']) {
    const client = harness(async (path) => response(200, path.endsWith('/proofs') ? {id: 'proof1', code: '12345678', status: 'pending'} : {status: phase}));
    const parent = new Element('div');
    await client.proofFlow(parent, 'provider.write', 'p1', () => assert.fail('must not commit'));
    await parent.byText('Request approval for this change').click();
    await parent.byText('Check approval').click();
    assert.equal(parent.byText('Submit the reviewed change'), undefined);
  }
});

test('destructive actions require the named consequence confirmation', async () => {
  const client = harness();
  let commits = 0;
  const parent = new Element('div');
  client.confirmChange(parent, 'Revoke Alice’s device', 'Revoke Alice’s browser and all descendant credentials.', async () => { commits++; });
  const submit = parent.ofTag('button')[0];
  await submit.click();
  assert.equal(commits, 0);
  parent.ofTag('input')[0].checked = true;
  await submit.click();
  assert.equal(commits, 1);
});

test('selectors load all pages and detect repeated cursors', async () => {
  const calls = [];
  const client = harness(async path => {
    calls.push(path);
    return response(200, calls.length === 1 ? {items: [{id: 'u1'}], next_cursor: 'u1'} : {items: [{id: 'u2'}], next_cursor: null});
  });
  assert.deepEqual(plain(await client.selectionRows('/users')), [{id: 'u1'}, {id: 'u2'}]);
  assert.match(calls[1], /cursor=u1/);
  const stuck = harness(async () => response(200, {items: [], next_cursor: 'same'}));
  await assert.rejects(stuck.selectionRows('/users'), /invalid page cursor/);
});

test('grant editor preserves unknown current selections instead of silently widening or removing restrictions', () => {
  const client = harness();
  const parent = new Element('div');
  const current = {profile_ids: ['p-unlisted'], permissions: ['catalog:read'], policy: {library_ids: ['l-unlisted'], allow_unrated: false, deny_rated: true, allowed_ratings: ['PG'], blocked_labels: ['blocked']}};
  const read = client.grantFields(parent, current, {profiles: [], libraries: []});
  assert.deepEqual(plain(read()), current);
});

test('ordinary account page only requests self-scoped data and public methods', async () => {
  const calls = [];
  const client = harness(async path => {
    calls.push(path);
    return response(200, path.endsWith('/auth/me') ? {user: {id: 'u1', name: 'Alice'}, identities: [], sessions: []} : {providers: [], email_enabled: false, items: [], next_cursor: null});
  });
  await client.selfPage(new Element('div'), new Element('p'));
  assert.deepEqual(calls.sort(), ['/api/v2/auth/identities?limit=200', '/api/v2/auth/me', '/api/v2/auth/methods', '/api/v2/auth/sessions?limit=200']);
  assert.ok(calls.every(path => !path.includes('/admin/') && !path.includes('/users')));
});

test('public sign-in reads safe discovery and never loads provider administration', async () => {
  const calls = [];
  const client = harness(async path => { calls.push(path); return response(200, {providers: [{id: 'p1', name: '<script>bad</script>'}], email_enabled: true}); });
  const content = new Element('div');
  await client.signInPage(content, new Element('p'));
  assert.deepEqual(calls, ['/api/v2/auth/methods']);
  assert.ok(content.byText('Continue with <script>bad</script>'));
  assert.equal(content.ofTag('script').length, 0);
});

test('OIDC callback transaction query shows terminal status and follows signed-in local return only', async () => {
  for (const phase of ['pending_approval', 'tested', 'linked', 'expired', 'denied', 'signed_in']) {
    let assigned;
    const calls = [];
    const client = harness(async path => {
      calls.push(path);
      return response(200, path.endsWith('/auth/methods') ? {providers: [], email_enabled: false} : {status: phase, redirect_to: phase === 'signed_in' ? '/settings/signin' : null});
    }, {location: {origin: 'https://motion.test', search: '?transaction=tx-1', assign(value) { assigned = value; }}});
    const content = new Element('div');
    await client.signInPage(content, new Element('p'));
    assert.ok(calls.includes('/api/v2/auth/transactions/tx-1'));
    if (phase === 'signed_in') assert.equal(assigned, 'https://motion.test/settings/signin');
    else assert.equal(assigned, undefined);
    if (phase === 'tested') assert.ok(content.byText('Return to Authentication & Access'));
    if (phase === 'linked') assert.ok(content.byText('Manage my sign-in methods'));
    if (phase === 'pending_approval') assert.ok([...content.descendants()].some(element => element.textContent.includes('No application access has been granted')));
  }
});

const settle = () => new Promise(resolve => setTimeout(resolve, 0));
async function submit(form) { form.events.submit({preventDefault() {}}); for (let i = 0; i < 5; i++) await settle(); }
const forms = root => root.ofTag('form');
const inputs = root => root.ofTag('input');

test('registration is offered only for a non-closed policy and states what verification does', async () => {
  for (const [registration, offered] of [['closed', false], ['open', true], ['approval_required', true], ['invite_only', true]]) {
    const client = harness(async () => response(200, {providers: [], email_enabled: true, registration}));
    const content = new Element('div');
    await client.signInPage(content, new Element('p'));
    const panelTitle = content.byText('Create an account');
    assert.equal(Boolean(panelTitle), offered, registration);
    if (registration === 'invite_only') assert.ok([...content.descendants()].some(e => e.textContent.includes('require an invitation')));
    if (registration === 'approval_required') assert.ok([...content.descendants()].some(e => e.textContent.includes('reviews each request before any access')));
  }
  const disabled = harness(async () => response(200, {providers: [], email_enabled: false, registration: 'open'}));
  const content = new Element('div');
  await disabled.signInPage(content, new Element('p'));
  assert.equal(content.byText('Create an account'), undefined);
});

test('registration verifies by code in this browser and distinguishes pending approval from an active account', async () => {
  for (const [status, redirect, expectContinue] of [['account_active', '/', true], ['invitation_accepted', '/', true], ['pending_approval', null, false]]) {
    const calls = [];
    const client = harness(async (path, options) => {
      calls.push({path, body: options.body && JSON.parse(options.body)});
      if (path.endsWith('/auth/register/start')) return response(200, {transaction_id: 'tx-9', message: 'If this address can register, a verification code has been sent.'});
      return response(200, {status, session_issued: Boolean(redirect), redirect_to: redirect});
    });
    const parent = new Element('div');
    client.registrationFlow(parent, 'approval_required');
    const [address, name] = inputs(parent);
    address.value = ' person@example.com '; name.value = 'Person';
    await submit(forms(parent)[0]);
    assert.deepEqual(calls[0], {path: '/api/v2/auth/register/start', body: {address: 'person@example.com', name: 'Person'}});
    assert.equal(address.value, '');
    const code = inputs(parent).find(input => input.attributes.autocomplete === 'one-time-code');
    code.value = '12345678';
    await submit(forms(parent)[0]);
    assert.deepEqual(calls[1], {path: '/api/v2/auth/email/tx-9/finish', body: {code: '12345678'}});
    assert.equal(code.value, '', 'the code is not retained');
    const outcome = [...parent.descendants()].find(e => e.attributes['data-registration-outcome']);
    assert.equal(outcome.attributes['data-registration-outcome'], status);
    assert.equal(Boolean(parent.byText('Continue to Motion')), expectContinue, status);
    if (status === 'pending_approval') assert.match(outcome.textContent, /No access has been granted/);
  }
});

function emailHarness(email, calls) {
  return harness(async (path, options) => {
    calls.push({path, method: options.method, body: options.body && JSON.parse(options.body)});
    if (path === '/api/v2/admin/auth/email') return response(200, email);
    return response(200, {items: [], next_cursor: null});
  });
}

test('direct delivery settings submit DKIM key handling but never relay secrets', async () => {
  const calls = [];
  const email = {revision: '4', enabled: false, mode: 'direct', from: 'Motion <motion@example.com>', host: '', port: 587, has_password: true,
    direct: {hostname: 'mail.example.com', envelope_from: 'bounces@example.com', selector: 'motion1', loopback_port: null, network_requirement: 'port 25'},
    dkim: {has_key: true, key_id: 'abcd', record_name: 'motion1._domainkey.example.com', record: 'v=DKIM1; k=rsa; p=AAA'}};
  const client = emailHarness(email, calls);
  const content = new Element('div');
  await client.emailSection(content);
  assert.ok(content.byText('TXT motion1._domainkey.example.com'));
  assert.ok(content.byText('v=DKIM1; k=rsa; p=AAA'));
  const save = forms(content).find(f => f.byText('Save email settings'));
  await submit(save);
  const put = calls.find(call => call.method === 'PUT');
  assert.equal(put.path, '/api/v2/admin/auth/email');
  assert.equal(put.body.mode, 'direct');
  assert.deepEqual(put.body.dkim_key, {action: 'retain'});
  assert.deepEqual(put.body.direct, {hostname: 'mail.example.com', envelope_from: 'bounces@example.com', selector: 'motion1', loopback_port: null});
  assert.equal(put.body.password, undefined);
  assert.equal(put.body.host, undefined);
  assert.ok(calls.some(call => call.path.startsWith('/api/v2/admin/auth/email/activity')));
});

test('relay settings keep secret retain semantics and send no DKIM fields', async () => {
  const calls = [];
  const email = {revision: '2', enabled: false, mode: 'relay', from: 'motion@example.com', host: 'smtp.example.com', port: 587, username: 'u', has_password: true, direct: {}, dkim: {has_key: false}};
  const client = emailHarness(email, calls);
  const content = new Element('div');
  await client.emailSection(content);
  await submit(forms(content).find(f => f.byText('Save email settings')));
  const put = calls.find(call => call.method === 'PUT');
  assert.deepEqual(put.body.password, {action: 'retain'});
  assert.equal(put.body.dkim_key, undefined);
  assert.equal(put.body.direct, undefined);
  assert.equal(content.byText('Diagnostics'), undefined, 'diagnostics are for direct delivery');
});

test('delivery activity shows status words, domains and possible duplicates without addresses', async () => {
  const client = harness(async path => path === '/api/v2/admin/auth/email'
    ? response(200, {revision: '1', enabled: true, mode: 'relay', from: 'm@example.com', direct: {}, dkim: {}})
    : response(200, {items: [{id: 'm1', purpose: 'registration_code', status: 'acceptance_unknown', attempts: 6, max_attempts: 6, possible_duplicate: true, recipient_domain: 'example.org', created_at: 1, mode: 'relay', configuration_revision: '1', detail: 'final reply timeout'}], next_cursor: null}));
  const content = new Element('div');
  await client.emailSection(content);
  assert.ok(content.byText('Registration code · example.org'));
  assert.ok(content.byText('Acceptance unknown (may have been delivered)'));
  assert.ok(content.byText('An earlier attempt may already have been delivered.'));
});

test('registration policy editor never offers administrator authority as a default', async () => {
  const calls = [];
  const client = harness(async (path, options) => {
    calls.push({path, method: options.method, body: options.body && JSON.parse(options.body)});
    return response(200, {policy: 'open', revision: '3', default_access: {profile_ids: [], permissions: ['catalog:read'], policy: {library_ids: []}}});
  }, {location: {origin: 'https://motion.test', search: '', reload() {}, assign() {}}});
  const content = new Element('div');
  await client.registrationSection(content, {profiles: [], libraries: [], users: []});
  assert.equal(content.byText('Administer this server'), undefined);
  assert.ok(content.byText('Browse catalog'));
  await submit(forms(content)[0]);
  const put = calls.find(call => call.method === 'PUT');
  assert.equal(put.body.policy, 'open');
  assert.deepEqual(put.body.default_access.permissions, ['catalog:read']);
});

const owner = {id: 'u1', name: 'Owner', revision: '1', disabled: false, profile_ids: [], permissions: ['catalog:read', 'playback:request', 'system:admin'], policy: {library_ids: [], allow_unrated: true}};
const pendingPairing = {id: 'p1', device_name: 'Web browser', client_name: 'Motion web', status: 'pending', user_code: 'ABCD-EFGH', expires_at: '2026-10-14T00:10:00Z'};
function enrollmentHarness(calls) {
  return harness(async (path, options) => {
    calls.push({path, method: options.method, body: options.body && JSON.parse(options.body)});
    if (path.startsWith('/api/v2/auth/pairings?')) return response(200, {items: [pendingPairing], next_cursor: null});
    if (path.startsWith('/api/v2/auth/pairings/')) return response(200, {id: 'd1'});
    return response(200, {items: [], next_cursor: null});
  });
}
const checkboxFor = (root, label) => root.byText(label).parent.children[0];

test('the only enabled user is preselected without administration, which needs deliberate confirmation', async () => {
  const calls = [];
  const client = enrollmentHarness(calls);
  const content = new Element('div');
  await client.enrollmentSection(content, {users: [owner], profiles: [], libraries: [], setup: {owner_setup: 'complete', pairing_user: owner}});
  const form = forms(content)[0];
  assert.equal(form.ofTag('select')[0].value, 'u1');
  assert.ok(content.byText('Preselected: Owner is the only enabled user.'));
  assert.equal(checkboxFor(form, 'Administer this server').checked, false);
  assert.equal(checkboxFor(form, 'Play media').checked, true);
  const code = inputs(form).find(input => input.attributes.autocomplete === 'off');
  code.value = 'ABCD-EFGH';
  checkboxFor(form, 'Administer this server').checked = true;
  await submit(form);
  assert.equal(calls.filter(call => call.method === 'POST').length, 0, 'administration without confirmation is not sent');
  checkboxFor(form, 'If Administer this server is selected: I deliberately grant administration to this device.').checked = true;
  await submit(form);
  const approve = calls.find(call => call.method === 'POST');
  assert.equal(approve.path, '/api/v2/auth/pairings/p1/approve');
  assert.equal(approve.body.user_id, 'u1');
  assert.equal(approve.body.user_code, 'ABCD-EFGH');
  assert.ok(approve.body.permissions.includes('system:admin'));
  assert.deepEqual(plain(client.inheritedGrant(owner)).permissions, ['catalog:read', 'playback:request']);
});

test('several users are chosen explicitly and no users point to owner setup', async () => {
  const second = {...owner, id: 'u2', name: 'Sam', permissions: ['catalog:read']};
  const content = new Element('div');
  await enrollmentHarness([]).enrollmentSection(content, {users: [owner, second], profiles: [], libraries: [], setup: {owner_setup: 'existing_users', pairing_user: null}});
  assert.equal(forms(content)[0].ofTag('select')[0].value, '');
  assert.equal(checkboxFor(content, 'Administer this server').checked, false);

  const empty = new Element('div');
  await enrollmentHarness([]).enrollmentSection(empty, {users: [], profiles: [], libraries: [], setup: {owner_setup: 'required', pairing_user: null}});
  assert.equal(empty.byText('Approve this device'), undefined);
  assert.ok(empty.byText('No user exists yet. Create the Owner under Users first, then approve this device for it.'));
});

test('owner setup is offered only on a server without users and approves no device', async () => {
  const calls = [];
  const client = harness(async (path, options) => {
    calls.push({path, method: options.method, body: options.body && JSON.parse(options.body)});
    if (options.method === 'POST') return response(201, {owner_setup: 'complete'});
    return response(200, {items: [], next_cursor: null});
  });
  const content = new Element('div');
  await client.usersSection(content, {users: [], profiles: [], libraries: [], setup: {owner_setup: 'required'}});
  assert.ok(content.byText('First run: create the Owner'));
  const setupForm = forms(content).find(form => form.byText('Create the Owner'));
  await submit(setupForm);
  const post = calls.find(call => call.method === 'POST');
  assert.equal(post.path, '/api/v2/access/setup');
  assert.deepEqual(post.body, {name: 'Owner'});
  for (const state of ['complete', 'existing_users']) {
    const other = new Element('div');
    await harness(async () => response(200, {items: [], next_cursor: null})).usersSection(other, {users: [], profiles: [], libraries: [], setup: {owner_setup: state}});
    assert.equal(other.byText('First run: create the Owner'), undefined, state);
  }
});

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
  return runInNewContext(`${source}\n({api, mutation, secretUpdate, secretField, safeRedirect, errorMessage, revision, node, proofFlow, selectionRows, grantFields, confirmChange, signInPage, selfPage, pending, emailFlow})`, {
    document, location, fetch, crypto: webcrypto, URL, URLSearchParams, TextEncoder,
    navigator: {}, ...extra,
  });
}
const response = (status, data) => ({ok: status >= 200 && status < 300, status, json: async () => data});
const plain = value => JSON.parse(JSON.stringify(value));

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

import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {runInNewContext} from 'node:vm';
import {webcrypto} from 'node:crypto';

const source = readFileSync(new URL('./bridge.js', import.meta.url), 'utf8');
function harness(storage = new Map(), fetch = async () => ({ok: true, status: 204}), extra = {}) {
  const context = {
    document: {querySelector: () => null, addEventListener() {}},
    sessionStorage: {getItem: key => storage.get(key), setItem: (key, value) => storage.set(key, value)},
    crypto: webcrypto, fetch, location: {reload() {}}, setTimeout: fn => fn(), Blob, ...extra,
  };
  return runInNewContext(`${source}\n({submitCommand, keyFor, settleKey, bodyOf, pair, showPlan, edited})`, context);
}
function form(inputs = []) {
  const status = {};
  const button = {setAttribute() {}, removeAttribute() {}};
  return {dataset: {command: 'POST /api/v2/libraries/lib1/scans', idempotent: 'true'},
    classList: {contains: () => false}, querySelectorAll: () => inputs,
    querySelector: selector => selector === '.command-status' ? Object.assign(status, {setAttribute() {}}) : button};
}
const input = (name, type, value, checked = false) => ({name, dataset: {type}, value, checked});

test('uncertain outcomes retain keys across edits and reload; success retires them', async () => {
  const storage = new Map();
  const keys = [];
  const fetch = async (_, options) => {
    keys.push(options.headers['Idempotency-Key']);
    return {ok: false, status: 503, json: async () => ({})};
  };
  const bridge = harness(storage, fetch);
  const field = input('mode', 'text', 'incremental');
  const command = form([field]);
  await bridge.submitCommand(command);
  field.value = 'full';
  await bridge.submitCommand(command);
  field.value = 'incremental';
  await harness(storage, fetch).submitCommand(command);
  assert.equal(keys[0], keys[2]);
  assert.notEqual(keys[0], keys[1]);
  await harness(storage).submitCommand(command);
  await harness(storage, fetch).submitCommand(command);
  assert.notEqual(keys[0], keys[3]);
  // A fresh page must not reuse the successfully settled key.
  await harness(storage, fetch).submitCommand(form([input('mode', 'text', 'full')]));
  assert.equal(keys[1], keys.at(-1));
});

test('corrupt storage values cannot break commands or supply invalid keys', () => {
  for (const raw of ['null', '[]', '42', '{', '{"request":42}']) {
    const bridge = harness(new Map([['motion:pending-idempotency-keys', raw]]));
    assert.match(bridge.keyFor('request'), /^[0-9a-f]{32}$/);
  }
});

test('rejection retires keys but timeout, rate limit and server failure preserve them', async () => {
  for (const status of [400, 401, 403, 408, 409, 412, 425, 429, 500, 503]) {
    const keys = [];
    const bridge = harness(new Map(), async (_, options) => {
      keys.push(options.headers['Idempotency-Key']);
      return {ok: false, status, json: async () => ({})};
    });
    const command = form();
    await bridge.submitCommand(command);
    await bridge.submitCommand(command);
    assert.equal(keys[0] === keys[1], [408, 425, 429, 500, 503].includes(status), `status ${status}`);
  }
});

test('changed preconditions identify a different request', async () => {
  const keys = [];
  const bridge = harness(new Map(), async (_, options) => {
    keys.push(options.headers['Idempotency-Key']);
    throw new Error('lost response');
  });
  const command = form();
  command.dataset.ifMatch = '"r1"';
  await bridge.submitCommand(command);
  command.dataset.ifMatch = '"r2"';
  await bridge.submitCommand(command);
  command.dataset.ifMatch = '"r1"';
  await bridge.submitCommand(command);
  assert.notEqual(keys[0], keys[1]);
  assert.equal(keys[0], keys[2]);
});

test('duplicate submit events run only one request until it settles', async () => {
  let release;
  let count = 0;
  const bridge = harness(new Map(), () => { count++; return new Promise(resolve => { release = resolve; }); });
  const command = form();
  const first = bridge.submitCommand(command);
  await bridge.submitCommand(command);
  assert.equal(count, 1);
  release({ok: true, status: 204});
  await first;
});

test('unavailable storage retains retry identity in memory', () => {
  const bridge = harness({get() { throw new Error('unavailable'); }, set() { throw new Error('unavailable'); }});
  const key = bridge.keyFor('request');
  assert.equal(bridge.keyFor('request'), key);
  bridge.settleKey('request');
  assert.notEqual(bridge.keyFor('request'), key);
});

test('encoding failure releases the submission guard for a corrected form', async () => {
  let count = 0;
  const bridge = harness(new Map(), async () => { count++; return {ok: true, status: 204}; });
  const field = input('source_ids', 'json', '{');
  const command = form([field]);
  await bridge.submitCommand(command);
  assert.equal(count, 0);
  field.value = '[]';
  await bridge.submitCommand(command);
  assert.equal(count, 1);
});

test('library source list is present even when empty and includes checked members', () => {
  const bridge = harness();
  const fields = [input('source_ids', 'json', '[]'), input('source_ids', 'member', 's1', true), input('source_ids', 'member', 's2')];
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields))), '{"source_ids":["s1"]}');
  fields[1].checked = false;
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields))), '{"source_ids":[]}');
});

test('pairing retries session exchange without reclaiming the credential', async () => {
  const paths = [];
  const bridge = harness(new Map(), async path => {
    paths.push(path);
    if (paths.length === 2) throw new Error('connection lost');
    return {ok: true, status: 200, json: async () => ({access_token: 'ephemeral'})};
  });
  await bridge.pair(form(), {id: 'pair1', user_code: 'CODE', device_code: 'secret', poll_interval_seconds: 0, expires_at: new Date(Date.now() + 10000).toISOString()});
  assert.deepEqual(paths, ['/api/v2/auth/pairings/pair1/claim', '/api/v2/auth/session', '/api/v2/auth/session']);
});

test('dotted names nest, blank optional fields are omitted and minutes become seconds', () => {
  const bridge = harness();
  const fields = [
    input('values', 'json', '{}'),
    input('values.title', 'optional', 'Heat'),
    input('values.description', 'optional', '  '),
    input('values.release_year', 'optional-int', '1995'),
    input('tracks.audio_track_id', 'nullable', ''),
    input('tracks.subtitle_policy', 'text', 'off'),
    Object.assign(input('interval_minutes', 'minutes', '90'), {dataset: {type: 'minutes', target: 'interval_seconds'}}),
  ];
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields))),
    '{"values":{"title":"Heat","release_year":1995},"tracks":{"audio_track_id":null,"subtitle_policy":"off"},"interval_seconds":5400}');
  // An emptied correction removes that field from the local contribution.
  fields[1].value = '';
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields)).values), '{"release_year":1995}');
});

test('precomputed bodies are sent exactly and identify the request with the precondition', async () => {
  const sent = [];
  const bridge = harness(new Map(), async (path, options) => { sent.push([path, options]); return {ok: true, status: 200, json: async () => ({})}; });
  const command = form();
  command.dataset = {command: 'PUT /api/v2/collections/c1', idempotent: 'false', ifMatch: '"r-3"', body: 'json'};
  const fixed = {value: '{"name":"Weekend","profile_id":"p","kind":"manual","item_ids":["i1"],"filter_id":null}'};
  const base = command.querySelector;
  command.querySelector = selector => selector === '[data-json-body]' ? fixed : base(selector);
  await bridge.submitCommand(command);
  assert.equal(sent[0][0], '/api/v2/collections/c1');
  assert.equal(sent[0][1].body, fixed.value);
  assert.equal(sent[0][1].headers['If-Match'], '"r-3"');
  assert.equal(sent[0][1].headers['Content-Type'], 'application/json');
});

test('file uploads send the raw bytes as octet-stream with a retry-stable key', async () => {
  const sent = [];
  const bridge = harness(new Map(), async (_, options) => { sent.push(options); return {ok: false, status: 503, json: async () => ({})}; });
  const file = new Blob([new Uint8Array([0x89, 0x50, 0x4e, 0x47])]);
  Object.assign(file, {name: 'poster.png', lastModified: 1});
  const command = form();
  command.dataset = {command: 'POST /api/v2/catalog/items/i1/artwork', idempotent: 'true', body: 'file'};
  const base = command.querySelector;
  const picker = {files: [file]};
  command.querySelector = selector => selector === 'input[type="file"]' ? picker : base(selector);
  await bridge.submitCommand(command);
  await bridge.submitCommand(command);
  assert.equal(sent[0].body, file);
  assert.equal(sent[0].headers['Content-Type'], 'application/octet-stream');
  assert.equal(sent[0].headers['Idempotency-Key'], sent[1].headers['Idempotency-Key']);
  // No file chosen: nothing is sent.
  picker.files = [];
  await bridge.submitCommand(command);
  assert.equal(sent.length, 2);
});

test('a previewed plan is committed only on confirmation, with its own token and key', async () => {
  const sent = [];
  let reloaded = 0;
  const elements = [];
  const element = () => {
    const node = {children: [], attributes: {}, listeners: {}, textContent: '',
      append(...nodes) { this.children.push(...nodes); }, remove() {},
      setAttribute(name, value) { this.attributes[name] = value; }, removeAttribute(name) { delete this.attributes[name]; },
      addEventListener(type, fn) { this.listeners[type] = fn; }};
    elements.push(node);
    return node;
  };
  const bridge = harness(new Map(), async (path, options) => {
    sent.push([path, options]);
    return {ok: true, status: 200, json: async () => ({})};
  }, {document: {querySelector: () => null, addEventListener() {}, createElement: element}, location: {reload() { reloaded++; }}});
  const status = {setAttribute() {}};
  const host = {querySelector: selector => selector === '.command-status' ? status
      : selector === '[data-plan-commit]' ? {dataset: {planCommit: 'POST /api/v2/sources/s1/relocation'}} : null,
    append() {}};
  bridge.showPlan(host, {plan_token: 'token-0123456789abcdef', expires_at: new Date().toISOString(), affected_ids: ['a', 'b'], warnings: ['2 files were not found']}, {new_root_path: '/srv/new'});
  assert.equal(sent.length, 0, 'previewing never commits');
  const confirm = elements.find(e => e.textContent === 'Apply this change');
  assert.ok(elements.some(e => e.textContent === '2 files were not found'));
  assert.ok(elements.some(e => e.textContent.startsWith('Move to /srv/new: 2 items affected.')));
  await confirm.listeners.click();
  assert.equal(sent.length, 1);
  assert.equal(sent[0][0], '/api/v2/sources/s1/relocation');
  assert.equal(sent[0][1].method, 'POST');
  assert.equal(sent[0][1].body, '{"plan_token":"token-0123456789abcdef"}');
  assert.match(sent[0][1].headers['Idempotency-Key'], /^[0-9a-f]{32}$/);
  assert.equal(reloaded, 1);
});

test('a new preview withdraws the previous plan before it is sent, even if it fails', async () => {
  let removed = 0;
  const bridge = harness(new Map(), async () => ({ok: false, status: 422, json: async () => ({detail: 'invalid root'})}));
  const command = form([input('new_root_path', 'text', '/srv/other')]);
  command.dataset = {command: 'POST /api/v2/sources/s1/relocation-plans', idempotent: 'true', ifMatch: '"r-2"'};
  command.classList = {contains: name => name === 'plan'};
  const base = command.querySelector;
  command.querySelector = selector => selector === '.plan-result' ? {remove() { removed++; }} : base(selector);
  await bridge.submitCommand(command);
  assert.equal(removed, 1);
});

test('a preview answered after the form was edited is discarded, not offered', async () => {
  let release;
  const bridge = harness(new Map(), () => new Promise(resolve => { release = resolve; }));
  const status = {setAttribute() {}};
  let shown = 0;
  const command = form([input('new_root_path', 'text', '/srv/a')]);
  command.dataset = {command: 'POST /api/v2/sources/s1/relocation-plans', idempotent: 'true', ifMatch: '"r-2"'};
  command.classList = {contains: name => name === 'plan'};
  command.append = () => { shown++; };
  const base = command.querySelector;
  command.querySelector = selector => selector === '.command-status' ? status : selector === '.plan-result' ? null : base(selector);
  const pending = bridge.submitCommand(command);
  bridge.edited(command);
  release({ok: true, status: 200, json: async () => ({plan_token: 'token-0123456789abcdef', expires_at: new Date().toISOString(), affected_ids: [], warnings: []})});
  await pending;
  assert.equal(shown, 0, 'the stale plan is never rendered');
  assert.equal(status.textContent, 'The form changed while checking. Check again.');
});

test('desktop-only download actions are revealed only inside the desktop host', () => {
  for (const [agent, revealed] of [['Mozilla/5.0 Chrome/140 MotionDesktop/1', true], ['Mozilla/5.0 Chrome/140', false]]) {
    const action = {hidden: true};
    runInNewContext(source, {
      document: {querySelector: () => null, querySelectorAll: selector => selector === '[data-desktop-only]' ? [action] : [], addEventListener() {}},
      navigator: {userAgent: agent}, crypto: webcrypto,
    });
    assert.equal(action.hidden, !revealed);
  }
});

test('filter terms are typed and only chosen criteria are sent', () => {
  const bridge = harness();
  const term = (field, operator, valueType, value) =>
    ({name: 'all', dataset: {type: 'term', field, operator, valueType}, value});
  const body = bridge.bodyOf(form([
    input('name', 'text', 'Unwatched 90s'),
    term('kind', 'eq', 'text', 'movie'),
    term('title', 'contains', 'text', ''),
    term('year', 'gte', 'int', '1990'),
    term('year', 'lte', 'int', ' 1999 '),
    term('watched', 'eq', 'bool', 'false'),
    term('availability', 'eq', 'bool', ''),
  ]));
  assert.deepEqual(JSON.parse(JSON.stringify(body)), {name: 'Unwatched 90s', all: [
    {field: 'kind', operator: 'eq', value: 'movie'},
    {field: 'year', operator: 'gte', value: 1990},
    {field: 'year', operator: 'lte', value: 1999},
    {field: 'watched', operator: 'eq', value: false},
  ]});
  // No criteria at all is an empty conjunction, not a missing field.
  assert.deepEqual(JSON.parse(JSON.stringify(bridge.bodyOf(form([term('title', 'contains', 'text', '')])))), {all: []});
});

test('seconds are sent as whole milliseconds and blank as null', () => {
  const bridge = harness();
  const body = bridge.bodyOf(form([input('start_ms', 'seconds-ms', '12.3456'), input('end_ms', 'seconds-ms', ' ')]));
  assert.deepEqual(JSON.parse(JSON.stringify(body)), {start_ms: 12346, end_ms: null});
});

test('a custom video profile sends whole bit rates and a chosen subtitle requires it', () => {
  const bridge = harness();
  const subtitle = Object.assign(input('tracks.subtitle_track_id', 'subtitle-choice', 's1'),
    {dataset: {type: 'subtitle-choice', policy: 'tracks.subtitle_policy'}});
  const fields = [
    input('recipe_id', 'text', 'video_profile'),
    input('video_profile.version', 'number', '1'),
    input('video_profile.max_width', 'number', '1920'),
    input('video_profile.video_bitrate', 'mbps', '8.5'),
    input('video_profile.audio_bitrate', 'kbps', '192'),
    input('video_profile.frame_rate', 'json', '{"numerator":24000,"denominator":1001}'),
    subtitle,
  ];
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields))),
    '{"recipe_id":"video_profile","video_profile":{"version":1,"max_width":1920,"video_bitrate":8500000,'
    + '"audio_bitrate":192000,"frame_rate":{"numerator":24000,"denominator":1001}},'
    + '"tracks":{"subtitle_track_id":"s1","subtitle_policy":"require"}}');
  // Floating-point input still yields whole bits per second.
  fields[3].value = '0.064';
  assert.equal(bridge.bodyOf(form(fields)).video_profile.video_bitrate, 64000);
  subtitle.value = '';
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields)).tracks), '{"subtitle_track_id":null,"subtitle_policy":"off"}');
});

test('after a command reloads the page, focus returns to the same action, never to another entry', () => {
  const storage = new Map();
  const focused = [];
  // A form acting on one entry: its command, its fixed entry field, and the
  // disclosure (if any) it sits in.
  const makeForm = (command, entry, details = null) => {
    const status = {textContent: ''};
    const button = {focus: () => focused.push(`${command} ${entry}`)};
    const hidden = [{name: 'entry', value: entry}];
    return {dataset: {command}, status, parentElement: details,
      querySelectorAll: s => s.startsWith('input[type="hidden"]') ? hidden : [],
      querySelector: s => s === '.command-status' ? status : s.startsWith('button') ? button : null};
  };
  let forms = [];
  const main = {focus: () => focused.push('main')};
  const context = (pathname = '/organize') => ({
    document: {querySelector: () => null, querySelectorAll: () => forms, getElementById: id => id === 'main' ? main : null, addEventListener() {}},
    sessionStorage: {getItem: k => storage.get(k), setItem: (k, v) => storage.set(k, v), removeItem: k => storage.delete(k)},
    crypto: webcrypto, location: {pathname, search: '', reload() {}}, setTimeout: fn => fn(), Blob,
  });
  const reorder = 'PUT /api/v2/playlists/p1/entries';
  forms = [makeForm(reorder, 'A'), makeForm(reorder, 'B')];
  const before = runInNewContext(`${source}\n({reloadAfter})`, context());
  // "Move up B": after the reload B is first, and its own action gets focus.
  before.reloadAfter(forms[1]);
  forms = [makeForm(reorder, 'B'), makeForm(reorder, 'A')];
  runInNewContext(source, context());
  assert.deepEqual(focused, [`${reorder} B`]);
  assert.equal(forms[0].status.textContent, 'Done.');
  assert.equal(forms[1].status.textContent, '', 'never another entry');
  assert.equal(storage.size, 0, 'used once');
  // "Remove B": B is gone, so the main content, never A's action.
  before.reloadAfter(forms[0]);
  forms = [makeForm(reorder, 'A')];
  runInNewContext(source, context());
  assert.deepEqual(focused.at(-1), 'main');
  // Two look-alike forms: ambiguous, so the main content.
  before.reloadAfter(forms[0]);
  forms = [makeForm(reorder, 'A'), makeForm(reorder, 'A')];
  runInNewContext(source, context());
  assert.deepEqual(focused.at(-1), 'main');
  // A form inside a closed disclosure is shown again before focus.
  const details = {tagName: 'DETAILS', open: false, parentElement: null};
  forms = [makeForm('PUT /api/v2/libraries/l1', 'l1', details)];
  before.reloadAfter(forms[0]);
  forms = [makeForm('PUT /api/v2/libraries/l1', 'l1', details)];
  runInNewContext(source, context());
  assert.equal(details.open, true);
  assert.deepEqual(focused.at(-1), 'PUT /api/v2/libraries/l1 l1');
  // A revision precondition that every success advances is not identity:
  // "Place" is found again after its group revision moved on.
  const place = (revision) => {
    const form = makeForm('POST /api/v2/catalog/order-groups/g1/placements', 'g1');
    const fields = [{name: 'timeline_id', value: 't1'}, {name: 'expected_group_revision', value: revision}];
    form.querySelectorAll = s => s.startsWith('input[type="hidden"]') ? fields : [];
    return form;
  };
  forms = [place('3')];
  before.reloadAfter(forms[0]);
  forms = [place('4')];
  runInNewContext(source, context());
  assert.deepEqual(focused.at(-1), 'POST /api/v2/catalog/order-groups/g1/placements g1');
  assert.equal(forms[0].status.textContent, 'Done.');
  // Another page: nothing is moved.
  const count = focused.length;
  before.reloadAfter(forms[0]);
  runInNewContext(source, context('/elsewhere'));
  assert.equal(focused.length, count);
});

test('invitation redemption retains claim after lost response without storing the code', async () => {
  const storage = new Map(), bodies = [], paths = [];
  let first = true;
  const bridge = harness(storage, async (path, options) => {
    paths.push(path); bodies.push(JSON.parse(options.body));
    if (first) {first = false; throw new Error('connection lost');}
    return {ok: true, status: 200, json: async () => ({access_token: 'credential'})};
  });
  const f = form([input('code','text','private-invitation'),input('device_name','text','Browser'),input('client_name','text','Motion web')]);
  f.classList.contains = cls => cls === 'invitation';
  await bridge.submitCommand(f); await bridge.submitCommand(f);
  assert.equal(paths.length, 3);
  assert.equal(paths[2], '/api/v2/auth/session');
  assert.deepEqual(bodies[0], bodies[1]);
  assert.match(bodies[0].claim_id, /^[a-f0-9]{32}$/);
  assert.equal(storage.size, 0);
});

test('denied and expired pairing stop polling and explain the actual outcome', async () => {
  for (const [code, expected] of [
    ['pairing_denied', 'An administrator denied this pairing request. Ask them before starting a new request.'],
    ['pairing_expired', 'The pairing code expired before it was approved. Start a new pairing request.'],
  ]) {
    let calls = 0;
    const bridge = harness(new Map(), async () => {
      calls++;
      return {ok:false, status:409, json:async()=>({code,detail:'Server detail'})};
    });
    const f = form();
    await bridge.pair(f, {id:'pair',user_code:'CODE',device_code:'secret',poll_interval_seconds:0,expires_at:new Date(Date.now()+1000).toISOString()});
    assert.equal(calls,1);
    assert.equal(f.querySelector('.command-status').textContent, expected);
  }
});

test('an unavailable invitation asks for a new code without saving the secret', async () => {
  const storage = new Map();
  const bridge = harness(storage, async () => ({ok:false, status:409, json:async()=>({code:'invitation_unavailable'})}));
  const f = form([input('code', 'text', 'private-invitation')]);
  f.classList.contains = cls => cls === 'invitation';
  await bridge.submitCommand(f);
  assert.equal(f.querySelector('.command-status').textContent, 'This invitation is no longer available. Ask an administrator for a new code.');
  assert.equal(storage.size, 0);
});

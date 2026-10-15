// Authentication & Access presentation. Every decision is rechecked by the
// server. Credentials, proofs and configuration secrets are never persisted in
// browser storage; rendered text always goes through textContent.
const API = '/api/v2';
const permissions = [
  ['catalog:read', 'Browse catalog'], ['catalog:write', 'Edit catalog'],
  ['sources:manage', 'Manage sources'], ['profiles:manage', 'Manage profiles'],
  ['viewing:write', 'Save viewing progress'], ['playback:request', 'Play media'],
  ['processing:request', 'Request processing'], ['downloads:manage', 'Manage downloads'],
  ['collections:write', 'Manage collections'], ['events:read', 'Receive updates'],
  ['system:admin', 'Administer this server'],
];
const pending = new Map();
let fieldSequence = 0;
const idPath = value => encodeURIComponent(String(value));
const rows = data => Array.isArray(data?.items) ? data.items : [];
const revision = item => `"r-${item.revision}"`;
const when = value => value ? new Date(typeof value === 'number' ? value * 1000 : value).toLocaleString() : 'Unknown';

function node(tag, text, attributes = {}) {
  const element = document.createElement(tag);
  if (text !== undefined && text !== null) element.textContent = String(text);
  for (const [key, value] of Object.entries(attributes)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === 'class') element.className = value;
    else if (key === 'checked' || key === 'disabled' || key === 'hidden') element[key] = Boolean(value);
    else element.setAttribute(key, String(value));
  }
  return element;
}
function button(label, run, parent, className = '') {
  const control = node('button', label, {type: 'button', class: className});
  control.addEventListener('click', run);
  parent?.append(control);
  return control;
}
function status(parent, message = '') {
  const output = node('p', message, {class: 'command-status', role: 'status', 'aria-live': 'polite'});
  parent.append(output);
  return output;
}
function announce(output, message, error = false) {
  output.textContent = message;
  output.setAttribute('role', error ? 'alert' : 'status');
  if (error) { output.setAttribute('tabindex', '-1'); output.focus({preventScroll: true}); }
}
function detail(parent, label, value) {
  const line = node('p', undefined, {class: 'access-detail'});
  line.append(node('strong', `${label}: `), node('span', value ?? 'Unknown'));
  parent.append(line);
}
function section(parent, id, title, text) {
  const element = node('section', undefined, {class: 'access-section', id, 'aria-labelledby': `${id}-heading`});
  element.append(node('h2', title, {id: `${id}-heading`}));
  if (text) element.append(node('p', text, {class: 'muted'}));
  parent.append(element);
  return element;
}
function panel(parent, title, subtitle) {
  const item = node('article', undefined, {class: 'access-card'});
  item.append(node('h3', title));
  if (subtitle) item.append(node('p', subtitle, {class: 'muted'}));
  parent.append(item);
  return item;
}
function field(parent, label, value = '', type = 'text', attributes = {}) {
  const id = `access-field-${++fieldSequence}`;
  const wrapper = node('div', undefined, {class: 'access-field'});
  wrapper.append(node('label', label, {for: id}));
  const input = node('input', undefined, {id, type, ...attributes});
  input.value = value ?? '';
  wrapper.append(input); parent.append(wrapper);
  return input;
}
function select(parent, label, options, selected = '') {
  const id = `access-field-${++fieldSequence}`;
  const wrapper = node('div', undefined, {class: 'access-field'});
  wrapper.append(node('label', label, {for: id}));
  const control = node('select', undefined, {id});
  for (const [value, text] of options) control.append(node('option', text, {value}));
  control.value = selected;
  wrapper.append(control); parent.append(wrapper);
  return control;
}
function checkbox(parent, label, checked = false) {
  const wrapper = node('label', undefined, {class: 'access-check'});
  const input = node('input', undefined, {type: 'checkbox', checked});
  wrapper.append(input, node('span', label)); parent.append(wrapper);
  return input;
}
function choices(parent, title, options, selected = []) {
  const group = node('fieldset', undefined, {class: 'access-choices'});
  group.append(node('legend', title));
  const all = new Map(options);
  for (const id of selected) if (!all.has(id)) all.set(id, `${id} (currently selected)`);
  const controls = [...all].map(([id, label]) => [id, checkbox(group, label, selected.includes(id))]);
  if (!controls.length) group.append(node('p', 'None available.', {class: 'muted'}));
  parent.append(group);
  return () => controls.filter(([, input]) => input.checked).map(([id]) => id);
}
function disclosure(parent, title) {
  const element = node('details', undefined, {class: 'access-disclosure'});
  element.append(node('summary', title)); parent.append(element);
  return element;
}
function secretField(parent, label, present) {
  const group = node('fieldset'); group.append(node('legend', label));
  group.append(node('p', present ? 'A secret is stored. Its value is never returned to this browser.' : 'No secret is stored.', {class: 'muted'}));
  const action = select(group, `${label} action`, [['retain', 'Retain current value'], ['replace', 'Replace with a new value'], ['clear', 'Clear stored value']], 'retain');
  const value = field(group, `New ${label.toLowerCase()}`, '', 'password', {autocomplete: 'new-password', maxlength: 8192});
  value.disabled = true;
  action.addEventListener('change', () => { value.disabled = action.value !== 'replace'; value.required = !value.disabled; if (value.disabled) value.value = ''; });
  parent.append(group);
  return {read: () => secretUpdate(action.value, value.value), clear: () => { value.value = ''; }};
}
function secretUpdate(action, value) {
  if (!['retain', 'replace', 'clear'].includes(action)) throw Object.assign(new Error('Choose how to update the secret.'), {status: 400});
  if (action === 'replace' && !value) throw Object.assign(new Error('Enter the replacement secret.'), {status: 400});
  return action === 'replace' ? {action, value} : {action};
}
function safeRedirect(value, external = false) {
  const url = new URL(value, location.origin);
  const local = url.origin === location.origin;
  const loopback = ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname);
  if (url.username || url.password || (!local && !external) || !(['https:'].includes(url.protocol) || (url.protocol === 'http:' && (local || loopback)))) {
    throw new Error('The server returned an unsafe redirect. Start again.');
  }
  return url.href;
}
function errorMessage(error) {
  if (error.code === 'recent_auth_required') return 'This change needs fresh authentication or approval from another authorized device.';
  if (error.code === 'invalid_code') return 'That code did not match. Check the email and enter the code in the browser that requested it.';
  if (error.code === 'code_attempts_exhausted') return 'This code has reached its attempt limit. Start a new email request.';
  if (['signin_expired', 'signin_already_used'].includes(error.code)) return 'This sign-in request expired or was already used. Start a new request.';
  if (error.code === 'registration_closed') return 'This server does not accept new account registrations.';
  if (error.code === 'registration_changed') return 'Registration settings changed. Start a new registration.';
  if (error.code === 'registration_unavailable') return 'This registration cannot be completed. If you already have an account, sign in by email instead.';
  if (error.code === 'registration_capacity') return 'Registration is temporarily unavailable. Try again later.';
  if (error.code === 'registration_admin_forbidden') return 'Open registration can never grant administrator access. Remove that permission.';
  if (error.code === 'email_test_required') return 'Save the settings disabled, send a successful test, then enable.';
  if (error.code === 'last_signin_method') return 'Keep another usable sign-in method before removing this one, or arrange recovery with the local operator.';
  if (error.code === 'last_administrator') return 'Keep another usable administrator before disabling or removing this access. Set up and verify another administrator first.';
  if ([409, 412, 428].includes(error.status)) return 'This information changed or the request expired. Refresh this section, review the current values, and submit a new request.';
  if (error.status === 401) return 'Your session ended. Sign in again to continue.';
  if (error.status === 403) return 'Your current access does not allow this action.';
  if (error.status === 410) return 'This request expired or was already used. Start a new request.';
  if (error.status === 429) return 'Too many attempts. Wait before trying again.';
  if (error.status >= 500 || !error.status) return 'The server did not confirm the outcome. Refresh before trying again; do not assume the action succeeded.';
  return error.message || 'The request was rejected. Check the entered values.';
}
async function api(method, path, body, options = {}) {
  const url = new URL(`${API}${path}`, location.origin);
  if (url.origin !== location.origin || !url.pathname.startsWith(`${API}/`)) throw new Error('Invalid API destination.');
  const headers = {Accept: 'application/json'};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (method !== 'GET') {
    headers['X-CSRF-Token'] = document.querySelector('meta[name="motion-csrf"]')?.content ?? '';
    if (options.key) headers['Idempotency-Key'] = options.key;
    if (options.revision !== undefined) headers['If-Match'] = revision({revision: options.revision});
    if (options.proof) headers['X-Motion-Action-Proof'] = options.proof;
  }
  const response = await fetch(url.pathname + url.search, {method, headers, credentials: 'same-origin', redirect: 'error', cache: 'no-store', body: body === undefined ? undefined : JSON.stringify(body)});
  const data = response.status === 204 ? null : await response.json().catch(() => null);
  if (!response.ok) throw Object.assign(new Error(data?.detail || data?.title || 'The request was rejected.'), {status: response.status, code: data?.code});
  return data;
}
async function withBusy(control, output, run) {
  if (control.disabled) return;
  control.disabled = true; announce(output, 'Working…');
  try { return await run(); }
  catch (error) { announce(output, errorMessage(error), true); }
  finally { control.disabled = false; }
}
// An approval is action-bound and single-use. No automatic replay: the user
// must review the original change and explicitly submit it after approval.
async function proofFlow(parent, action, target, retry) {
  const group = panel(parent, 'Approve this sensitive change', 'Use a different authorized administrator device to compare and approve the code. Refreshing this page is not fresh authentication.');
  const output = status(group);
  const request = button('Request approval for this change', () => withBusy(request, output, async () => {
    const proof = await api('POST', '/auth/proofs', {action, target});
    request.remove();
    detail(group, 'Approval code', proof.code);
    detail(group, 'Expires', when(proof.expires_at));
    const check = button('Check approval', () => withBusy(check, output, async () => {
      const current = await api('GET', `/auth/proofs/${idPath(proof.id)}`);
      announce(output, `Approval: ${current.status}.`);
      if (current.status === 'approved') {
        check.remove();
        const commit = button('Submit the reviewed change', () => withBusy(commit, output, async () => { await retry(proof.id); group.remove(); }), group);
      }
    }), group);
    announce(output, 'Waiting for another authorized device or the local operator.');
  }), group);
}
async function mutation(parent, method, path, body, options = {}) {
  const bytes = new TextEncoder().encode(JSON.stringify([method, path, body, options.revision]));
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  const identity = Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, '0')).join('');
  const key = pending.get(identity) ?? crypto.randomUUID();
  pending.set(identity, key);
  const perform = async proof => {
    try {
      const result = await api(method, path, body, {...options, key, proof});
      pending.delete(identity);
      await options.done?.(result);
      return result;
    } catch (error) {
      if (error.status && error.status < 500 && ![408, 425, 429].includes(error.status)) pending.delete(identity);
      throw error;
    }
  };
  try { return await perform(options.proof); }
  catch (error) {
    if (error.code === 'recent_auth_required' && options.action) {
      await proofFlow(parent, options.action, options.target ?? 'new', perform);
    } else if ([409, 412, 428].includes(error.status) && !['last_administrator', 'last_signin_method'].includes(error.code)) {
      const stale = panel(parent, 'Review the latest state', 'This change was not applied. Refresh to see current authority and revisions before submitting again.');
      button('Refresh and review', () => location.reload(), stale);
    }
    throw error;
  }
}
function form(parent, label, run) {
  const element = node('form', undefined, {class: 'access-form'});
  const fields = node('div', undefined, {class: 'access-fields'});
  element.append(fields);
  const submit = node('button', label, {type: 'submit'}); element.append(submit);
  const output = status(element);
  element.addEventListener('submit', event => { event.preventDefault(); void withBusy(submit, output, async () => { await run(fields, output); }); });
  parent.append(element);
  return {element, fields, output, submit};
}
function confirmChange(parent, label, consequence, run) {
  const area = disclosure(parent, label);
  area.append(node('p', consequence, {class: 'notice'}));
  const confirmed = checkbox(area, 'I understand this change and its scope.');
  const output = status(area);
  const submit = button(label, () => withBusy(submit, output, async () => {
    if (!confirmed.checked) { announce(output, 'Confirm the consequence before continuing.', true); return; }
    await run(output);
  }), area, 'access-danger');
  return area;
}
async function paged(parent, path, render, emptyText = 'Nothing to show.') {
  const list = node('div', undefined, {class: 'access-list'}); parent.append(list);
  const output = status(parent, 'Loading…');
  let cursor;
  const more = button('Load more', () => withBusy(more, output, load), parent);
  more.hidden = true;
  async function load() {
    const query = new URLSearchParams({limit: '50'});
    if (cursor) query.set('cursor', cursor);
    const data = await api('GET', `${path}${path.includes('?') ? '&' : '?'}${query}`);
    const items = rows(data);
    for (const item of items) render(list, item);
    cursor = data?.next_cursor;
    more.hidden = !cursor;
    announce(output, !list.children.length ? emptyText : cursor ? 'More results are available.' : 'All results shown.');
  }
  await load().catch(error => { announce(output, errorMessage(error), true); });
  return list;
}
async function selectionRows(path) {
  const result = [];
  const seen = new Set();
  let cursor;
  do {
    const query = new URLSearchParams({limit: '200'});
    if (cursor) query.set('cursor', cursor);
    const data = await api('GET', `${path}?${query}`);
    result.push(...rows(data));
    cursor = data?.next_cursor;
    if (cursor && seen.has(cursor)) throw Object.assign(new Error('The server returned an invalid page cursor. Refresh the page.'), {status: 409});
    if (cursor) seen.add(cursor);
  } while (cursor);
  return result;
}
// The grant an approval starts from: the user's grant without administration,
// which is only ever granted deliberately.
function inheritedGrant(user) {
  if (!user) return {};
  return {profile_ids: user.profile_ids ?? [], permissions: (user.permissions ?? []).filter(id => id !== 'system:admin'), policy: user.policy ?? {}};
}
function grantFields(parent, current = {}, lookups = {}, permissionOptions = permissions) {
  const profile_ids = choices(parent, 'Allowed viewing profiles', (lookups.profiles ?? []).map(v => [v.id, v.name]), current.profile_ids ?? []);
  const permission_ids = choices(parent, 'Permissions', permissionOptions, current.permissions ?? []);
  const policy = current.policy ?? {};
  const library_ids = choices(parent, 'Allowed libraries', (lookups.libraries ?? []).map(v => [v.id, v.name]), policy.library_ids ?? []);
  parent.append(node('p', 'No libraries selected means no library access. Administrator grants remain subject to the user and device ceilings.', {class: 'muted small'}));
  const allow_unrated = checkbox(parent, 'Allow media without a rating', policy.allow_unrated ?? true);
  const deny_rated = checkbox(parent, 'Deny all rated media', policy.deny_rated ?? false);
  const ratings = field(parent, 'Allowed ratings (comma separated; blank allows any rating)', (policy.allowed_ratings ?? []).join(', '));
  const labels = field(parent, 'Blocked labels (comma separated)', (policy.blocked_labels ?? []).join(', '));
  const split = input => input.value.split(',').map(value => value.trim()).filter(Boolean);
  return () => ({profile_ids: profile_ids(), permissions: permission_ids(), policy: {library_ids: library_ids(), allow_unrated: allow_unrated.checked, deny_rated: deny_rated.checked, allowed_ratings: split(ratings), blocked_labels: split(labels)}});
}
async function startOidc(parent, provider, purpose) {
  const output = status(parent);
  const control = button(purpose === 'login' ? `Continue with ${provider.name}` : purpose === 'test' ? 'Test browser sign-in' : purpose === 'reauth' ? `Confirm account with ${provider.name}` : `Link ${provider.name}`, () => withBusy(control, output, async () => {
    await mutation(parent, 'POST', `/auth/oidc/${idPath(provider.id)}/start`, {purpose, return_to: purpose === 'test' ? '/settings/access' : ['link', 'reauth'].includes(purpose) ? '/settings/signin' : '/'}, {
      action: purpose === 'test' ? 'provider.test' : purpose === 'link' ? 'identity.link' : undefined,
      target: provider.id,
      done: result => { location.assign(safeRedirect(result.authorization_url, true)); },
    });
  }), parent);
}
function emailFlow(parent, purpose = 'login') {
  const heading = purpose === 'enroll' ? 'Enroll an email sign-in method' : purpose === 'reauth' ? 'Confirm account by email' : 'Sign in by email';
  const area = panel(parent, heading, 'Enter the one-time code in this browser. An email address alone never links an existing account.');
  const send = form(area, 'Send sign-in code', async () => {
    const enteredAddress = address.value.trim();
    await mutation(area, 'POST', '/auth/email/start', {address: enteredAddress, purpose}, {
      action: purpose === 'enroll' ? 'identity.link' : undefined, target: 'email',
      done: response => {
        address.value = ''; send.element.remove();
        area.append(node('p', response.message || 'If this address can sign in, a code has been sent.'));
        const verify = form(area, 'Verify code', async () => {
          const result = await api('POST', `/auth/email/${idPath(response.transaction_id)}/finish`, {code: code.value.trim()});
          code.value = '';
          if (result.redirect_to) location.assign(safeRedirect(result.redirect_to));
          else announce(verify.output, result.status === 'pending_approval' ? 'Awaiting administrator approval. No application access has been granted.' : 'Email verified.');
        });
        const code = field(verify.fields, 'One-time email code', '', 'text', {autocomplete: 'one-time-code', inputmode: 'numeric', required: true, maxlength: 12});
        code.focus();
        button('Start a new email request', () => { area.remove(); emailFlow(parent, purpose); }, area);
      },
    });
  });
  const address = field(send.fields, 'Email address', '', 'email', {autocomplete: 'email', required: true, maxlength: 320});
}
const registrationText = {
  open: 'Verify your email address to create an account. Your account becomes active immediately with the access this server grants new members.',
  approval_required: 'Verify your email address to request an account. An administrator reviews each request before any access is granted.',
  invite_only: 'Accounts on this server require an invitation sent to your email address. Use the address that received the invitation.',
};
const registrationOutcome = {
  account_active: 'Your account is active.',
  invitation_accepted: 'Invitation accepted. Your account is active.',
  pending_approval: 'Email verified. Your request is waiting for administrator approval. No access has been granted yet; you will be able to sign in by email once it is approved.',
};
function registrationFlow(parent, policy) {
  const area = panel(parent, 'Create an account', registrationText[policy] ?? '');
  area.setAttribute('data-registration', policy);
  const send = form(area, 'Send verification code', async () => {
    const response = await api('POST', '/auth/register/start', {address: address.value.trim(), name: name.value.trim() || undefined});
    address.value = ''; send.element.remove();
    area.append(node('p', response.message || 'If this address can register, a verification code has been sent.', {role: 'status'}));
    const verify = form(area, 'Verify email', async () => {
      const result = await api('POST', `/auth/email/${idPath(response.transaction_id)}/finish`, {code: code.value.trim()});
      code.value = ''; verify.element.remove();
      const outcome = node('p', registrationOutcome[result.status] ?? 'Verification finished.', {class: 'notice', role: 'status'});
      outcome.setAttribute('data-registration-outcome', result.status);
      area.append(outcome);
      if (result.redirect_to) area.append(node('a', 'Continue to Motion', {href: safeRedirect(result.redirect_to), class: 'button primary'}));
      else if (result.status !== 'pending_approval') area.append(node('a', 'Sign in by email', {href: '/signin', class: 'button'}));
    });
    const code = field(verify.fields, 'Verification code', '', 'text', {autocomplete: 'one-time-code', inputmode: 'numeric', required: true, maxlength: 12});
    code.focus();
    button('Start again', () => { area.remove(); registrationFlow(parent, policy); }, area);
  });
  const address = field(send.fields, 'Email address', '', 'email', {autocomplete: 'email', required: true, maxlength: 320});
  const name = policy === 'open' || policy === 'approval_required' ? field(send.fields, 'Your name (shown to the administrator)', '', 'text', {autocomplete: 'name', maxlength: 100}) : {value: ''};
}
async function signInPage(content, output) {
  const data = await api('GET', '/auth/methods');
  const params = new URLSearchParams(location.search);
  if (params.get('transaction')) {
    const transaction = await api('GET', `/auth/transactions/${idPath(params.get('transaction'))}`);
    if (transaction.status === 'signed_in' && transaction.redirect_to) {
      location.assign(safeRedirect(transaction.redirect_to));
      return;
    }
    const descriptions = {
      ...registrationOutcome,
      pending_approval: 'An administrator must approve this identity. No application access has been granted. Sign in again after approval.',
      tested: 'Provider sign-in verified. Return to Authentication & Access to explicitly enable this provider.',
      linked: 'Sign-in method linked to your account.',
      expired: 'This sign-in request expired. Start a new request.',
      denied: 'Sign-in was denied. Start a new request when you are ready.',
      failed: 'Sign-in could not complete. Start a new request.',
      pending: 'Sign-in has not completed yet.',
    };
    content.append(node('p', transaction.detail || descriptions[transaction.status] || 'Sign-in status is unavailable.', {class: 'notice', role: 'status'}));
    if (transaction.status === 'tested') content.append(node('a', 'Return to Authentication & Access', {href: '/settings/access#methods', class: 'button primary'}));
    if (transaction.status === 'linked') content.append(node('a', 'Manage my sign-in methods', {href: '/settings/signin', class: 'button primary'}));
  }
  for (const provider of data.providers ?? []) await startOidc(content, provider, 'login');
  if (data.email_enabled) emailFlow(content);
  if (data.email_enabled && data.registration && data.registration !== 'closed') registrationFlow(content, data.registration);
  announce(output, data.providers?.length || data.email_enabled ? 'Use one of these methods or pair this browser below.' : 'Pair this browser to request access.');
}

function providerForm(parent, provider, reload) {
  const editing = Boolean(provider?.id);
  const area = disclosure(parent, editing ? `Edit ${provider.name}` : 'Add an OIDC provider');
  area.append(node('p', 'Save a disabled draft, test configuration, complete a browser sign-in test, then explicitly enable the provider.'));
  const saved = provider ?? {};
  const input = form(area, editing ? 'Save provider' : 'Save disabled draft', async () => {
    if (editing && saved.enabled && !enabled.checked && !disableConfirmed.checked) throw Object.assign(new Error('Confirm disabling this provider and its sessions.'), {status: 400});
    const destinations = destination.value.split('\n').map(line => line.trim()).filter(Boolean).map(line => {
      const [origin, address, ...extra] = line.split(/\s+/);
      if (extra.length) throw Object.assign(new Error('Use one destination origin and optional pinned address per line.'), {status: 400});
      return {origin, address: address || null};
    });
    const body = {name: name.value.trim(), issuer: issuer.value.trim(), client_id: client.value.trim(), enabled: editing && enabled.checked, destinations, client_secret: secret.read()};
    await mutation(area, editing ? 'PUT' : 'POST', `/admin/auth/providers${editing ? `/${idPath(saved.id)}` : ''}`, body, {revision: saved.revision, action: 'provider.write', target: saved.id ?? 'new', done: async () => { secret.clear(); await reload(); }});
  });
  const name = field(input.fields, 'Display name', saved.name, 'text', {required: true, maxlength: 100});
  const issuer = field(input.fields, 'Issuer URL', saved.issuer, 'url', {required: true, placeholder: 'https://identity.example.com'});
  const client = field(input.fields, 'Client ID', saved.client_id, 'text', {required: true, autocomplete: 'off'});
  const secret = secretField(input.fields, 'Client secret', saved.has_secret);
  const enabled = checkbox(input.fields, 'Enable this provider for sign-in', saved.enabled);
  enabled.disabled = !editing;
  const disableConfirmed = checkbox(input.fields, `If disabling ${saved.name || 'this provider'}, I confirm cancellation of its pending sign-ins and revocation of sessions authenticated through it.`);
  const wrapper = node('div', undefined, {class: 'access-field'});
  const did = `access-field-${++fieldSequence}`;
  wrapper.append(node('label', 'Allowed provider destinations (one HTTPS origin and optional pinned address per line)', {for: did}));
  const destination = node('textarea', undefined, {id: did, rows: 3, spellcheck: false});
  destination.value = (saved.destinations ?? []).map(v => `${v.origin}${v.address ? ` ${v.address}` : ''}`).join('\n');
  wrapper.append(destination); input.fields.append(wrapper);
  if (saved.callback_url) detail(input.fields, 'Exact callback URL', saved.callback_url);
}
async function methodsSection(parent) {
  const sectionRoot = section(parent, 'methods', 'Sign-in methods', 'Pairing remains available for enrollment and recovery if another sign-in provider is unavailable.');
  const body = node('div'); sectionRoot.append(body);
  const reload = async () => {
    const providers = await api('GET', '/admin/auth/providers');
    const email = await api('GET', '/admin/auth/email');
    body.replaceChildren();
    if (!rows(providers).length) body.append(node('p', 'No OIDC providers configured.', {class: 'muted'}));
    for (const provider of rows(providers)) {
      const area = panel(body, provider.name, provider.enabled ? 'Enabled' : 'Disabled draft');
      detail(area, 'Issuer', provider.issuer); detail(area, 'Callback URL', provider.callback_url);
      detail(area, 'Last configuration test', provider.last_test_at ? `${provider.last_test_ok ? 'Passed' : 'Failed'} · ${when(provider.last_test_at)}` : 'Not tested');
      detail(area, 'Last successful browser test', when(provider.last_login_at));
      if (provider.callback_url) {
        const copied = status(area);
        const copy = button('Copy callback URL', () => withBusy(copy, copied, async () => { await navigator.clipboard.writeText(provider.callback_url); announce(copied, 'Callback URL copied.'); }), area);
      }
      const output = status(area);
      const test = button('Test configuration', () => withBusy(test, output, async () => {
        await mutation(area, 'POST', `/admin/auth/providers/${idPath(provider.id)}/test`, {}, {action: 'provider.test', target: provider.id, done: reload});
      }), area);
      await startOidc(area, provider, 'test');
      providerForm(area, provider, reload);
      if (provider.enabled) confirmChange(area, 'Disable provider', `Disable ${provider.name}, cancel its pending sign-ins, and invalidate browser sessions authenticated through it. Independent pairing credentials retain their authority.`, async () => {
        const {name, issuer, client_id, destinations} = provider;
        await mutation(area, 'PUT', `/admin/auth/providers/${idPath(provider.id)}`, {name, issuer, client_id, destinations, enabled: false, client_secret: {action: 'retain'}}, {revision: provider.revision, action: 'provider.write', target: provider.id, done: reload});
      });
    }
    providerForm(body, null, reload);
    body.append(node('p', 'Passwordless email sign-in and its delivery are configured under Email delivery.', {class: 'muted'}));
  };
  await reload();
}

const deliveryStatus = {
  queued: 'Queued', retry_pending: 'Retry pending', sending: 'Sending', accepted: 'Accepted by mail server',
  failed: 'Failed', acceptance_unknown: 'Acceptance unknown (may have been delivered)', expired: 'Expired', cancelled: 'Cancelled',
};
const purposeLabel = {
  signin_code: 'Sign-in code', registration_code: 'Registration code', invitation: 'Invitation',
  account_notice: 'Existing-account notice', test: 'Delivery test',
};
function keyField(parent, dkim) {
  const group = node('fieldset'); group.append(node('legend', 'DKIM signing key'));
  group.append(node('p', dkim?.has_key ? `A key is stored (identity ${dkim.key_id}). The private key is never returned to this browser.` : 'No signing key is stored. Direct delivery requires one.', {class: 'muted'}));
  const action = select(group, 'DKIM key action', [['retain', 'Retain current key'], ['generate', 'Generate a new 2048-bit key'], ['import', 'Import a PEM private key'], ['clear', 'Remove the key']], 'retain');
  const wrapper = node('div', undefined, {class: 'access-field'});
  const id = `access-field-${++fieldSequence}`;
  wrapper.append(node('label', 'PEM private key to import (unencrypted RSA, 2048–4096 bits)', {for: id}));
  const pem = node('textarea', undefined, {id, rows: 4, spellcheck: false, autocomplete: 'off'});
  pem.disabled = true; wrapper.append(pem); group.append(wrapper);
  action.addEventListener('change', () => { pem.disabled = action.value !== 'import'; if (pem.disabled) pem.value = ''; });
  parent.append(group);
  return {
    read: () => {
      if (action.value === 'import' && !pem.value.trim()) throw Object.assign(new Error('Paste the PEM private key to import.'), {status: 400});
      return action.value === 'import' ? {action: 'import', value: pem.value} : {action: action.value};
    },
    clear: () => { pem.value = ''; },
  };
}
function recordList(parent, records) {
  const list = node('dl', undefined, {class: 'access-records'});
  for (const record of records) {
    list.append(node('dt', `${record.type} ${record.name}`));
    list.append(node('dd', record.value));
  }
  parent.append(list);
}
async function emailSection(parent) {
  const sectionRoot = section(parent, 'email', 'Email delivery', 'Delivery serves email sign-in, registration and invitations. Pairing and local recovery keep working during mail outages.');
  const body = node('div'); sectionRoot.append(body);
  const reload = async () => {
    const email = await api('GET', '/admin/auth/email');
    body.replaceChildren();
    const area = panel(body, 'Email sign-in and delivery', `${email.enabled ? 'Enabled' : 'Disabled'} · ${email.mode === 'direct' ? 'Native direct delivery' : 'SMTP relay'}`);
    detail(area, 'Configuration revision', email.revision);
    detail(area, 'Last delivery test', email.last_test_at ? `${email.last_test_ok ? 'Accepted by mail server' : 'Not accepted'} · ${when(email.last_test_at)}` : 'Not tested since the last change');
    detail(area, 'Confirmed receipt', email.last_receipt_at ? `Code entered · ${when(email.last_receipt_at)}` : 'Not confirmed for this configuration');
    area.append(node('p', 'Mail server acceptance is not proof of inbox delivery. Only entering the emailed test code confirms receipt, and no delivery result authenticates anyone.', {class: 'muted'}));
    if (email.mode === 'direct' && email.dkim?.record) {
      const records = panel(area, 'DNS records to publish', 'Publish these at your DNS provider. Motion cannot publish DNS records or set reverse DNS (PTR) for you.');
      recordList(records, [{type: 'TXT', name: email.dkim.record_name ?? 'selector._domainkey', value: email.dkim.record}]);
    }
    const edit = disclosure(area, 'Configure email delivery');
    const input = form(edit, 'Save email settings', async () => {
      if (email.enabled && !enabled.checked && !disableConfirmed.checked) throw Object.assign(new Error('Confirm disabling email sign-in and its sessions.'), {status: 400});
      const direct = mode.value === 'direct';
      const payload = {enabled: enabled.checked, mode: mode.value, from: from.value.trim(), apply_untested: applyUntested.checked};
      if (direct) {
        payload.direct = {hostname: hostname.value.trim(), envelope_from: envelope.value.trim(), selector: selector.value.trim(), loopback_port: loopbackPort.value ? Number(loopbackPort.value) : null};
        payload.dkim_key = key.read();
      } else {
        Object.assign(payload, {host: host.value.trim(), port: Number(port.value), username: username.value, loopback_test: loopback.checked, password: secret.read()});
      }
      await mutation(edit, 'PUT', '/admin/auth/email', payload, {revision: email.revision, action: 'email.write', target: 'email', done: async () => { secret.clear(); key.clear(); await reload(); }});
    });
    const mode = select(input.fields, 'Delivery mode', [['relay', 'SMTP relay (send through an existing mail server)'], ['direct', 'Native direct delivery (Motion connects to recipient mail servers)']], email.mode ?? 'relay');
    const from = field(input.fields, 'Sender (From)', email.from, 'text', {required: true, placeholder: 'Motion <motion@example.com>'});
    const relayGroup = node('fieldset'); relayGroup.append(node('legend', 'SMTP relay'));
    const host = field(relayGroup, 'Mail server hostname', email.host, 'text');
    const port = field(relayGroup, 'Port', email.port ?? 587, 'number', {min: 1, max: 65535});
    const username = field(relayGroup, 'Username', email.username, 'text', {autocomplete: 'off'});
    const secret = secretField(relayGroup, 'Mail password', email.has_password);
    const loopback = checkbox(relayGroup, 'Use an isolated local mail sink (loopback only)', email.loopback_test);
    input.fields.append(relayGroup);
    const directGroup = node('fieldset'); directGroup.append(node('legend', 'Native direct delivery'));
    directGroup.append(node('p', email.direct?.network_requirement ?? 'Direct delivery needs outbound TCP port 25.', {class: 'notice'}));
    const hostname = field(directGroup, 'Sending hostname (EHLO name; should match reverse DNS)', email.direct?.hostname, 'text', {placeholder: 'mail.example.com'});
    const envelope = field(directGroup, 'Envelope sender for bounces (an external mailbox you read)', email.direct?.envelope_from, 'email', {placeholder: 'bounces@example.com'});
    const selector = field(directGroup, 'DKIM selector', email.direct?.selector, 'text', {placeholder: 'motion1'});
    const key = keyField(directGroup, email.dkim);
    const loopbackPort = field(directGroup, 'Isolated loopback qualification port (testing only; plaintext to 127.0.0.1)', email.direct?.loopback_port ?? '', 'number', {min: 1, max: 65535});
    directGroup.append(node('p', 'TLS is required for every recipient server and never downgraded to plaintext. This is not MTA-STS or DANE.', {class: 'muted small'}));
    input.fields.append(directGroup);
    const showMode = () => { relayGroup.hidden = mode.value !== 'relay'; directGroup.hidden = mode.value !== 'direct'; };
    mode.addEventListener('change', showMode); showMode();
    const enabled = checkbox(input.fields, 'Enable email sign-in', email.enabled);
    const disableConfirmed = checkbox(input.fields, 'If disabling email, I confirm cancellation of pending email sign-ins and revocation of email-authenticated sessions.');
    const applyUntested = checkbox(input.fields, 'Apply changed delivery settings while email sign-in stays enabled. I will send a test immediately afterwards.');
    applyUntested.disabled = !email.enabled;
    input.fields.append(node('p', 'Saving changed delivery settings invalidates previous tests, cancels queued mail and signs out email-authenticated browser sessions.', {class: 'muted small'}));

    const testArea = panel(area, 'Send a test message', 'Choose a mailbox you control. The message carries a test code that cannot sign anyone in.');
    const testForm = form(testArea, 'Send test email', async () => {
      const address = destination.value.trim();
      await mutation(testArea, 'POST', '/admin/auth/email/test', {address}, {action: 'email.test', target: 'email', done: async result => {
        announce(testForm.output, `${deliveryStatus[result.status] ?? result.status}. ${result.detail ?? ''}`);
        const confirm = form(testArea, 'Confirm receipt', async () => {
          await mutation(testArea, 'POST', `/admin/auth/email/tests/${idPath(result.test_id)}/confirm`, {code: code.value.trim()}, {done: async () => { await reload(); }});
        });
        const code = field(confirm.fields, 'Test code from the received message', '', 'text', {inputmode: 'numeric', maxlength: 12, autocomplete: 'off', required: true});
      }});
    });
    const destination = field(testForm.fields, 'Explicit test email destination', '', 'email', {required: true, autocomplete: 'off'});

    if (email.mode === 'direct') {
      const diag = panel(area, 'Diagnostics', 'Checks published DNS records and, optionally, an outbound connection. Results are observations at one time, not guarantees.');
      const results = node('div');
      const run = form(diag, 'Run diagnostics', async () => {
        const report = await api('POST', '/admin/auth/email/diagnostics', {probe_domain: probe.value.trim() || null});
        results.replaceChildren();
        detail(results, 'Configuration revision', report.revision);
        detail(results, 'Observed', when(report.observed_at));
        const list = node('ul', undefined, {class: 'access-findings'});
        for (const finding of report.findings ?? []) {
          const item = node('li', undefined, {'data-status': finding.status});
          item.append(node('strong', `${finding.check}: ${finding.status}. `), node('span', finding.detail));
          list.append(item);
        }
        results.append(list);
        if (report.publish?.length) { results.append(node('h4', 'Records to publish')); recordList(results, report.publish); }
        results.append(node('h4', 'What Motion cannot prove'));
        const unknown = node('ul'); for (const text of report.cannot_prove ?? []) unknown.append(node('li', text));
        results.append(unknown);
        announce(run.output, 'Diagnostics complete.');
      });
      const probe = field(run.fields, 'Recipient domain to probe (optional, connects on port 25)', '', 'text', {placeholder: 'gmail.com', autocomplete: 'off'});
      diag.append(results);
    }
    const activity = panel(area, 'Delivery activity', 'Recent messages without addresses or codes. Retries reuse the same message; an unknown acceptance may produce a duplicate.');
    await paged(activity, '/admin/auth/email/activity', (host, item) => {
      const card = panel(host, `${purposeLabel[item.purpose] ?? item.purpose} · ${item.recipient_domain}`, deliveryStatus[item.status] ?? item.status);
      card.setAttribute('data-delivery-status', item.status);
      detail(card, 'Attempts', `${item.attempts} of ${item.max_attempts}`);
      detail(card, 'Created', when(item.created_at));
      if (item.next_attempt_at) detail(card, 'Next attempt', when(item.next_attempt_at));
      if (item.accepted_at) detail(card, 'Accepted', when(item.accepted_at));
      if (item.possible_duplicate) card.append(node('p', 'An earlier attempt may already have been delivered.', {class: 'muted'}));
      if (item.detail) detail(card, 'Last result', item.detail);
      detail(card, 'Mode and revision', `${item.mode} · revision ${item.configuration_revision}`);
    }, 'No messages yet.');
  };
  await reload();
}
async function registrationSection(parent, lookups) {
  const area = section(parent, 'registration', 'Registration', 'Choose what verifying an email address does for someone without an account. Verification proves control of an address; it never links an existing account or grants administration.');
  const current = await api('GET', '/admin/auth/registration');
  const descriptions = {
    closed: 'Closed: nobody can create an account. Use pairing, invitations redeemed on a device, or approve SSO requests.',
    approval_required: 'Approval required: verifying creates a pending access request. No access until you approve it for a user.',
    invite_only: 'Invite-only: only addresses with an unexpired emailed invitation can register; they receive the invitation grant.',
    open: 'Open: verifying creates an active account with the default access below, without manual approval.',
  };
  detail(area, 'Current policy', descriptions[current.policy] ?? current.policy);
  const edit = disclosure(area, 'Change registration policy');
  const input = form(edit, 'Save registration policy', async () => {
    await mutation(edit, 'PUT', '/admin/auth/registration', {policy: policy.value, default_access: grant()}, {revision: current.revision, action: 'registration.write', target: 'registration', done: () => location.reload()});
  });
  const policy = select(input.fields, 'Registration policy', Object.entries(descriptions).map(([value, text]) => [value, text.split(':')[0]]), current.policy);
  input.fields.append(node('p', 'Changing the policy ends registrations already in progress; they must start again.', {class: 'muted small'}));
  const defaults = node('fieldset'); defaults.append(node('legend', 'Default access for open registration'));
  const grant = grantFields(defaults, current.default_access ?? {}, lookups, permissions.filter(([id]) => id !== 'system:admin'));
  input.fields.append(defaults);
}
function ownerSetup(parent, lookups) {
  if (lookups.setup?.owner_setup !== 'required') return;
  const item = panel(parent, 'First run: create the Owner', 'This server has no users yet. The Owner is one enabled user whose access includes administration of this server.');
  item.append(node('p', 'Creating the Owner approves no device and does not change existing devices; unassigned devices stay unassigned. Approve each device, including a browser, under Pairing & invitations after comparing its code. Repeating this step returns the same Owner. Like creating a user, it needs recent sign-in or approval from another authorized device; on a new server, Motion terminal with the operator token needs neither.', {class: 'notice'}));
  const input = form(item, 'Create the Owner', async () => {
    await mutation(item, 'POST', '/access/setup', {name: name.value.trim() || 'Owner'}, {action: 'user.admin', target: 'owner', done: () => location.reload()});
  });
  const name = field(input.fields, 'Owner name', 'Owner', 'text', {required: true, maxlength: 100});
}
async function usersSection(parent, lookups) {
  const area = section(parent, 'users', 'Users', 'A local user is an access identity. Disabling access does not delete profiles or viewing history.');
  ownerSetup(area, lookups);
  function userForm(host, user) {
    const editing = Boolean(user);
    const edit = disclosure(host, editing ? 'Edit access and restrictions' : 'Create a local user');
    const current = user ?? {};
    const input = form(edit, editing ? 'Save user' : 'Create user', async () => {
      const body = {name: name.value.trim(), disabled: disabled.checked, ...grant()};
      if (editing && body.disabled && !current.disabled && !disableConfirm.checked) throw Object.assign(new Error('Confirm that all access for this user will be disabled.'), {status: 400});
      await mutation(edit, editing ? 'PUT' : 'POST', `/users${editing ? `/${idPath(current.id)}` : ''}`, body, {revision: current.revision, action: 'user.admin', target: current.id ?? 'new', done: () => location.reload()});
    });
    const name = field(input.fields, 'User name', current.name, 'text', {required: true, maxlength: 100});
    const disabled = checkbox(input.fields, 'Disable access', current.disabled);
    const grant = grantFields(input.fields, current, lookups);
    let disableConfirm;
    if (editing) {
      input.fields.append(node('p', `Disabling ${current.name} denies every owned device and session and invalidates pending enrollment. Re-enabling requires fresh authentication; revoked credentials stay revoked.`, {class: 'notice'}));
      disableConfirm = checkbox(input.fields, `I confirm disabling all access for ${current.name}, if selected above.`);
    }
  }
  await paged(area, '/users', (host, user) => {
    const item = panel(host, user.name, user.disabled ? 'Access disabled' : 'Access enabled');
    detail(item, 'User ID', user.id); detail(item, 'Permissions', user.permissions.join(', ') || 'None');
    userForm(item, user);
    const output = status(item);
    const inspect = button('Show devices and sign-in methods', () => withBusy(inspect, output, async () => {
      const details = await api('GET', `/admin/auth/users/${idPath(user.id)}`);
      const detailArea = panel(item, `${user.name} — access details`);
      for (const identity of details.identities ?? []) identityCard(detailArea, identity, true);
      for (const session of details.sessions ?? []) sessionCard(detailArea, session);
      for (const device of details.devices ?? []) detail(detailArea, 'Device', `${device.name} · ${device.revoked ? 'revoked' : 'active'}`);
      if (!(details.identities?.length || details.sessions?.length || details.devices?.length)) detailArea.append(node('p', 'No linked methods, sessions or devices.'));
      announce(output, 'Access details loaded.');
    }), item);
    confirmChange(item, 'Revoke all credentials', `Revoke all devices and browser sessions owned by ${user.name}. This does not delete viewing data.`, async () => {
      await mutation(item, 'POST', `/admin/auth/users/${idPath(user.id)}/revoke`, {}, {revision: user.revision, action: 'user.admin', target: user.id, done: () => location.reload()});
    });
  }, 'No local users. Create a user before granting access.');
  userForm(area);
}
async function requestsSection(parent, lookups) {
  const area = section(parent, 'requests', 'Sign-in access requests', 'These identities passed provider or email verification but have no application access. Email matches never link existing accounts. After approval the person signs in again.');
  await paged(area, '/admin/auth/requests', (host, request) => {
    const email = request.kind === 'email';
    const item = panel(host, email ? request.address : request.subject, `${email ? 'Verified email' : 'SSO identity'} · ${request.status}`);
    if (email) { detail(item, 'Verified address', request.address); detail(item, 'Name given', request.name ?? 'Not provided'); }
    else { detail(item, 'Issuer', request.issuer); detail(item, 'Subject', request.subject); }
    detail(item, 'Received', when(request.created_at)); detail(item, 'Expires', when(request.expires_at));
    if (request.status !== 'pending') return;
    const input = form(item, 'Approve verified identity', async () => {
      const chosen = lookups.users.find(user => user.id === owner.value);
      if (!chosen || !recovery.checked) throw Object.assign(new Error('Choose a user and confirm the explicit linking or recovery authorization.'), {status: 400});
      await mutation(item, 'POST', `/admin/auth/requests/${idPath(request.id)}/approve`, {user_id: chosen.id, user_revision: chosen.revision, recovery: true}, {revision: request.revision, action: 'request.approve', target: request.id, done: () => location.reload()});
    });
    const owner = select(input.fields, 'Local user receiving this identity', [['', 'Choose a user'], ...lookups.users.filter(user => !user.disabled).map(user => [user.id, user.name])]);
    input.fields.append(node('p', 'The identity receives no more than the selected user’s current grant and restrictions. Associating an existing user is an audited recovery action, not email matching.', {class: 'notice'}));
    const recovery = checkbox(input.fields, email ? 'I authorize this verified email address to sign in as the selected local user.' : 'I authorize this exact issuer and subject to link to the selected local user.');
    confirmChange(item, 'Deny request', email ? `Deny the access request from ${request.address}.` : `Deny the access request from ${request.subject} at ${request.issuer}.`, async () => {
      await mutation(item, 'POST', `/admin/auth/requests/${idPath(request.id)}/deny`, {}, {revision: request.revision, done: () => location.reload()});
    });
  }, 'No sign-in access requests.');
}
async function enrollmentSection(parent, lookups) {
  const area = section(parent, 'enrollment', 'Pairing & invitations', 'Enrollment grants access to a device. It does not link an email address or SSO identity to a user.');
  await paged(area, '/auth/pairings', (host, pairing) => {
    const item = panel(host, pairing.device_name, `${pairing.client_name} · ${pairing.status}`);
    detail(item, 'Expires', when(pairing.expires_at));
    if (pairing.status !== 'pending') return;
    detail(item, 'Requesting device code', pairing.user_code);
    // Denial never depends on an eligible owner.
    const deny = () => confirmChange(item, 'Deny pairing', `Deny the pending pairing from ${pairing.device_name}.`, async () => mutation(item, 'POST', `/auth/pairings/${idPath(pairing.id)}/deny`, undefined, {done: () => location.reload()}));
    const eligible = lookups.users.filter(user => !user.disabled);
    if (!eligible.length) {
      item.append(node('p', lookups.setup?.owner_setup === 'required' ? 'No user exists yet. Create the Owner under Users first, then approve this device for it.' : 'No enabled user. Create or enable a user before approving this device.', {class: 'notice'}));
      deny();
      return;
    }
    const input = form(item, 'Approve this device', async () => {
      const chosen = lookups.users.find(user => user.id === owner.value);
      if (!chosen) throw Object.assign(new Error('Choose an explicit local user.'), {status: 400});
      if (code.value.trim() !== pairing.user_code) throw Object.assign(new Error('Compare the code on the requesting device and enter it exactly.'), {status: 400});
      const requested = grant();
      if (requested.permissions.includes('system:admin') && !adminConfirm.checked) throw Object.assign(new Error('Administration is selected. Confirm that this device should administer the server, or clear Administer this server.'), {status: 400});
      await mutation(item, 'POST', `/auth/pairings/${idPath(pairing.id)}/approve`, {user_code: code.value.trim(), user_id: chosen.id, user_revision: chosen.revision, ...requested}, {action: 'pairing.approve', target: pairing.id, done: () => location.reload()});
    });
    // The only enabled user, as suggested by the server, is preselected.
    const suggested = eligible.length === 1 && lookups.setup?.pairing_user?.id === eligible[0].id ? eligible[0] : undefined;
    const owner = select(input.fields, 'Device owner', [['', 'Choose a user'], ...eligible.map(user => [user.id, user.name])], suggested?.id ?? '');
    if (suggested) input.fields.append(node('p', `Preselected: ${suggested.name} is the only enabled user.`, {class: 'muted small'}));
    const code = field(input.fields, 'Enter the code shown on the requesting device', '', 'text', {required: true, autocomplete: 'off'});
    const grantArea = node('div');
    input.fields.append(grantArea);
    let grant = grantFields(grantArea, inheritedGrant(suggested), lookups);
    owner.addEventListener('change', () => {
      grantArea.replaceChildren();
      grant = grantFields(grantArea, inheritedGrant(lookups.users.find(user => user.id === owner.value)), lookups);
    });
    input.fields.append(node('p', 'Administer this server is never preselected. It permits every operation, including managing users and devices.', {class: 'notice'}));
    const adminConfirm = checkbox(input.fields, 'If Administer this server is selected: I deliberately grant administration to this device.');
    deny();
  }, 'No pairing requests.');
  const invites = panel(area, 'Single-use invitations', 'Invitation codes appear only when created. Share them only with the intended recipient.');
  await paged(invites, '/auth/invitations', (host, invite) => {
    const item = panel(host, `Invitation for ${lookups.users.find(user => user.id === invite.user_id)?.name ?? invite.user_id}`, invite.status);
    detail(item, 'Expires', when(invite.expires_at)); detail(item, 'Permissions', invite.permissions.join(', ') || 'None');
    if (invite.email) detail(item, 'Emailed to', invite.email);
    if (invite.status === 'unused') confirmChange(item, 'Revoke invitation', `Invalidate this unused invitation for user ${invite.user_id}.`, async () => mutation(item, 'DELETE', `/auth/invitations/${idPath(invite.id)}`, undefined, {revision: invite.revision, done: () => location.reload()}));
  }, 'No invitations.');
  const create = disclosure(invites, 'Create an invitation');
  const input = form(create, 'Generate single-use invitation', async () => {
    const chosen = lookups.users.find(user => user.id === owner.value);
    if (!chosen) throw Object.assign(new Error('Choose an explicit local user.'), {status: 400});
    await mutation(create, 'POST', '/auth/invitations', {user_id: chosen.id, user_revision: chosen.revision, ttl_seconds: Number(ttl.value) * 60, email: recipient.value.trim() || null, ...grant()}, {action: 'invitation.create', target: chosen.id, done: result => {
      const created = panel(create, 'Invitation created', 'Copy this code now. It will not be displayed again.');
      detail(created, 'Invitation code', result.code); detail(created, 'Expires', when(result.expires_at));
      announce(input.output, 'Invitation created.');
    }});
  });
  const owner = select(input.fields, 'Invitation owner', [['', 'Choose a user'], ...lookups.users.filter(user => !user.disabled).map(user => [user.id, user.name])]);
  const ttl = field(input.fields, 'Expires in minutes', 60, 'number', {required: true, min: 1, max: 1440});
  const recipient = field(input.fields, 'Email the invitation to (optional; they register with this address)', '', 'email', {autocomplete: 'off', maxlength: 320});
  const grant = grantFields(input.fields, {}, lookups);
}
function identityCard(parent, identity, admin = false) {
  const title = identity.kind === 'email' ? identity.address : identity.provider_name || 'OIDC';
  const item = panel(parent, title, identity.kind);
  if (identity.subject) detail(item, 'Subject', identity.subject);
  if (identity.issuer) detail(item, 'Issuer', identity.issuer);
  if (identity.user_id && admin) detail(item, 'Owner', identity.user_id);
  confirmChange(item, 'Remove sign-in method', `Remove ${title} and invalidate browser sessions authenticated through it. The server prevents removal of the last viable sign-in method unless recovery is authorized.`, async () => {
    await mutation(item, 'DELETE', `/auth/identities/${idPath(identity.id)}`, undefined, {revision: identity.revision, action: 'identity.unlink', target: identity.id, done: () => location.reload()});
  });
}
function sessionCard(parent, session) {
  const item = panel(parent, session.device_name || 'Browser session', session.method);
  detail(item, 'Owner', session.user_name || session.user_id);
  detail(item, 'Issued', when(session.issued_at)); detail(item, 'Expires', when(session.expires_at));
  detail(item, 'Last activity', when(session.last_seen_at));
  detail(item, 'Device', session.device_id);
  confirmChange(item, 'Revoke browser session', `Sign out this ${session.method} browser session and revoke its renewal authority. Other sessions keep their own authority.`, async () => {
    await mutation(item, 'DELETE', `/auth/sessions/${idPath(session.id)}`, undefined, {revision: session.revision, done: () => location.reload()});
  });
}
async function sessionsSection(parent, lookups) {
  const area = section(parent, 'sessions', 'Devices & sessions', 'Browser sessions identify individual sign-ins, not verified physical devices. Activity not observed by the server is shown as unknown.');
  await paged(area, '/admin/auth/sessions', sessionCard, 'No browser sessions.');
  await paged(area, '/devices', (host, device) => {
    const item = panel(host, device.name, device.revoked ? 'Revoked device' : device.client_name);
    detail(item, 'Owner', lookups.users.find(user => user.id === device.user_id)?.name ?? device.user_id ?? 'Unowned legacy device');
    detail(item, 'Last activity', 'Unknown');
    detail(item, 'Permissions', device.permissions.join(', ') || 'None');
    if (!device.revoked) confirmChange(item, 'Revoke device', `Revoke ${device.name} and every credential or browser session descended from this device.`, async () => mutation(item, 'DELETE', `/devices/${idPath(device.id)}`, undefined, {revision: device.revision, done: () => location.reload()}));
    if (!device.revoked && !device.user_id) {
      const input = form(item, 'Assign an owner', async () => {
        const chosen = lookups.users.find(user => user.id === owner.value);
        if (!chosen) throw Object.assign(new Error('Choose a local user.'), {status: 400});
        await mutation(item, 'PUT', `/devices/${idPath(device.id)}/user`, {user_id: chosen.id, user_revision: chosen.revision}, {revision: device.revision, action: 'device.assign', target: device.id, done: () => location.reload()});
      });
      const owner = select(input.fields, 'New owner', [['', 'Choose a user'], ...lookups.users.filter(user => !user.disabled).map(user => [user.id, user.name])]);
    }
  }, 'No enrolled devices.');
}
async function activitySection(parent) {
  const area = section(parent, 'activity', 'Security activity', 'Sign-in outcomes and access changes. Tokens, codes, and provider secrets are excluded.');
  await paged(area, '/admin/auth/audit', (host, event) => {
    const item = panel(host, event.action, event.outcome);
    detail(item, 'Actor', event.actor); detail(item, 'Target', event.target); detail(item, 'Time', when(event.at));
  }, 'No security events.');
}
async function overviewSection(parent) {
  const area = section(parent, 'overview', 'Overview', 'New arrivals follow the registration policy. Verified email never links an existing account or grants administrator access.');
  const overview = await api('GET', '/admin/auth/overview');
  detail(area, 'Usable administrator devices', overview.usable_administrators);
  area.append(node('p', 'Several administrator devices can belong to one user. This count does not represent independent recovery accounts.', {class: 'muted'}));
  detail(area, 'Pending access requests', overview.pending_requests);
  detail(area, 'Public origin', overview.public_origin || 'Not configured');
  if (overview.restored_hold) area.append(node('p', 'New SSO and email sign-ins are held after restore. The local operator must review the restored authentication configuration before resuming them.', {class: 'notice', role: 'alert'}));
  for (const error of overview.configuration_errors ?? []) area.append(node('p', error, {role: 'alert'}));
  const edit = disclosure(area, 'Configure public origin');
  const input = form(edit, 'Save public origin', async () => mutation(edit, 'PUT', '/admin/auth/settings', {public_origin: origin.value.trim()}, {revision: overview.revision, action: 'settings.write', target: 'settings', done: () => location.reload()}));
  const origin = field(input.fields, 'Public Motion origin', overview.public_origin, 'url', {required: true, placeholder: 'https://motion.example.com'});
  const approvals = panel(area, 'Sensitive action approvals', 'Approve only a code compared with the requesting administrator on another authorized device.');
  await paged(approvals, '/admin/auth/fresh-approvals', (host, proof) => {
    const item = panel(host, proof.action, proof.status);
    detail(item, 'Target', proof.target); detail(item, 'Requested by', proof.actor || proof.device_id); detail(item, 'Expires', when(proof.expires_at));
    if (proof.status !== 'pending') return;
    const input = form(item, 'Approve this exact action', async () => mutation(item, 'POST', `/auth/proofs/${idPath(proof.id)}/approve`, {code: code.value.trim()}, {done: () => location.reload()}));
    const code = field(input.fields, 'Code shown on the requesting device', '', 'text', {required: true, autocomplete: 'off'});
  }, 'No pending sensitive action approvals.');
}
async function adminPage(content, output) {
  const [users, profiles, libraries, setup] = await Promise.all([selectionRows('/users'), selectionRows('/profiles'), selectionRows('/libraries'), api('GET', '/access/setup').catch(() => null)]);
  const lookups = {users, profiles, libraries, setup};
  const sections = [() => overviewSection(content), () => methodsSection(content), () => emailSection(content), () => registrationSection(content, lookups), () => usersSection(content, lookups), () => requestsSection(content, lookups), () => enrollmentSection(content, lookups), async () => {
    const area = section(content, 'identities', 'Linked identities', 'To add a method, the account owner uses My sign-in methods and completes fresh proof. Access requests can be explicitly approved as audited recovery; provider subjects are never reassigned by editing text.');
    area.append(node('a', 'Open My sign-in methods', {href: '/settings/signin', class: 'button'}));
    await paged(area, '/admin/auth/identities', (host, identity) => identityCard(host, identity, true), 'No linked identities.');
  }, () => sessionsSection(content, lookups), () => activitySection(content)];
  for (const render of sections) {
    try { await render(); }
    catch (error) { const retry = panel(content, 'Section unavailable'); const state = status(retry); announce(state, errorMessage(error), true); button('Reload access settings', () => location.reload(), retry); }
  }
  announce(output, 'Access settings loaded. Every change is checked against your current authority.');
}
async function selfPage(content, output) {
  const [me, methods] = await Promise.all([api('GET', '/auth/me'), api('GET', '/auth/methods')]);
  if (!me.user) {
    content.append(node('p', 'This legacy device has no assigned local user. An administrator must assign ownership before you can link a sign-in method.', {class: 'notice'}));
  } else {
    [me.identities, me.sessions] = await Promise.all([selectionRows('/auth/identities'), selectionRows('/auth/sessions')]);
    detail(content, 'Account', me.user.name);
    const linked = section(content, 'my-methods', 'Linked methods');
    for (const identity of me.identities ?? []) identityCard(linked, identity);
    if (!me.identities?.length) linked.append(node('p', 'No SSO or email method is linked. Pairing remains available.'));
    const add = section(content, 'add-method', 'Add a sign-in method', 'Linking requires fresh proof of your existing account and proof of the new method.');
    for (const provider of methods.providers ?? []) await startOidc(add, provider, 'link');
    if (methods.email_enabled) emailFlow(add, 'enroll');
    const fresh = section(content, 'confirm-account', 'Confirm your account', 'Sensitive changes require recent sign-in. Pairing-only accounts can request approval from another authorized device when submitting a change.');
    detail(fresh, 'Fresh authentication valid until', when(me.fresh_until));
    for (const provider of methods.providers ?? []) {
      if ((me.identities ?? []).some(identity => identity.provider_id === provider.id)) await startOidc(fresh, provider, 'reauth');
    }
    if (methods.email_enabled && (me.identities ?? []).some(identity => identity.kind === 'email')) emailFlow(fresh, 'reauth');
  }
  const sessions = section(content, 'my-sessions', 'My browser sessions');
  for (const session of me.sessions ?? []) sessionCard(sessions, session);
  if (!me.sessions?.length) sessions.append(node('p', 'No browser sessions.'));
  confirmChange(content, 'Sign out this browser', 'Invalidate this browser session and its renewal authority.', async () => {
    await api('DELETE', '/auth/session'); location.assign('/signin');
  });
  announce(output, 'Your sign-in methods and sessions are loaded.');
}

const root = document.querySelector('[data-access-page]');
if (root) {
  const output = root.querySelector('[data-access-status]');
  const content = root.querySelector('[data-access-content]');
  const render = root.dataset.accessPage === 'admin' ? adminPage : root.dataset.accessPage === 'self' ? selfPage : signInPage;
  render(content, output).catch(error => {
    announce(output, errorMessage(error), true);
    button('Reload', () => location.reload(), content);
    if (error.status === 401) content.append(node('a', 'Sign in again', {href: '/signin', class: 'button'}));
  });
}

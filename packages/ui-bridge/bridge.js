// Motion UI bridge: the one small external module on every server-rendered
// page. It holds no catalog or viewing state and never evaluates strings as
// code. It does three things:
//   1. submits command forms (<form data-command="METHOD /api/v2/...">) to the
//      public API with CSRF, a stable idempotency key and If-Match, then
//      reloads the server-rendered page;
//   2. runs browser pairing from the sign-in page;
//   3. switches the viewing profile among those the server rendered;
//   4. lets "Back" links (a[data-back]) return through history, since an
//      app installed to the home screen has no browser back button.
// Command forms may also preview a change plan and commit it only after the
// user has seen its effects (form.plan), or upload one file as the raw body
// (form[data-body="file"]).

const csrf = () => document.querySelector('meta[name="motion-csrf"]')?.content ?? '';
const newKey = () => crypto.randomUUID().replaceAll('-', '');

// Move focus to an error panel so keyboard and screen-reader users notice it.
const alert = document.querySelector('main > [role="alert"], main [role="alert"]');
if (alert) {
  alert.setAttribute('tabindex', '-1');
  alert.focus({preventScroll: true});
}

/** Set `value` at a dotted name ("tracks.audio_track_id") inside `body`. */
function place(body, name, value) {
  const path = name.split('.');
  const last = path.pop();
  let target = body;
  for (const key of path) {
    target[key] ??= {};
    target = target[key];
  }
  target[last] = value;
}

/** Encode a command form into the JSON body its operation expects. */
function bodyOf(form) {
  const body = {};
  let any = false;
  for (const input of form.querySelectorAll('[name][data-type]')) {
    const name = input.name;
    const type = input.dataset.type;
    if (input.type === 'radio' && !input.checked) continue;
    any = true;
    const text = input.value.trim();
    switch (type) {
      case 'bool': place(body, name, input.checked); break;
      case 'number': place(body, name, input.value === '' ? null : Number(input.value)); break;
      case 'list': place(body, name, input.value.split(',').map(v => v.trim()).filter(Boolean)); break;
      case 'json': place(body, name, JSON.parse(input.value)); break;
      case 'minutes': place(body, input.dataset.target, Math.round(Number(input.value) * 60)); break;
      // Blank optional fields are omitted: the server reads absence as "no value".
      case 'optional': if (text !== '') place(body, name, input.value); break;
      case 'optional-int': if (text !== '') place(body, name, Number(text)); break;
      case 'nullable': place(body, name, text === '' ? null : input.value); break;
      case 'member': {
        const path = name.split('.');
        let target = body;
        for (const key of path.slice(0, -1)) target = target[key] ??= {};
        target[path.at(-1)] ??= [];
        if (input.checked) target[path.at(-1)].push(input.value);
        break;
      }
      default: place(body, name, input.value);
    }
  }
  return any ? body : undefined;
}

/** One public API call; returns parsed JSON or throws an Error carrying the Problem. */
async function call(method, path, body, headers = {}) {
  const raw = typeof Blob !== 'undefined' && body instanceof Blob;
  const type = body === undefined ? {} : {'Content-Type': raw ? 'application/octet-stream' : 'application/json'};
  const response = await fetch(path, {
    method, credentials: 'same-origin', redirect: 'error', cache: 'no-store',
    headers: {'Accept': 'application/json', 'X-CSRF-Token': csrf(), ...type, ...headers},
    body: body === undefined ? undefined : raw ? body : JSON.stringify(body),
  });
  if (!response.ok) {
    const problem = await response.json().catch(() => null);
    const error = new Error(problem?.detail || problem?.title || `Request failed (${response.status})`);
    error.status = response.status;
    error.code = problem?.code ?? null;
    throw error;
  }
  return response.status === 204 ? null : response.json().catch(() => null);
}

function message(error) {
  if (error.status === 409 || error.status === 412) return 'This changed somewhere else. Reload the page to see the latest version, then try again.';
  if (error.status === 401) return 'Your session has ended. Sign in again.';
  if (error.status === 403) return 'You are not allowed to do that.';
  if (error.status === 428) return 'The page did not know the current version. Reload and try again.';
  if (error.status === 413) return 'That file is too large.';
  if (error.status === 415) return 'That kind of file is not accepted.';
  if (!error.status) return 'The server did not respond. Your request may or may not have been applied; submitting again is safe.';
  return error.message;
}

// Idempotency keys survive retries of the same request until its outcome is
// known: one key per (operation, body), kept across edits and page reloads in
// this tab, dropped only on success or a definite rejection.
const keyStore = 'motion:pending-idempotency-keys';
function pendingKeys() {
  try {
    const value = JSON.parse(sessionStorage.getItem(keyStore) ?? '{}');
    return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
  } catch { return {}; }
}
function savePendingKeys(keys) {
  try { sessionStorage.setItem(keyStore, JSON.stringify(keys)); } catch { /* storage unavailable: keys live for this page only */ }
}
const memoryKeys = {};
function keyFor(identity) {
  const stored = pendingKeys();
  const saved = stored[identity];
  const key = (typeof saved === 'string' && /^[0-9a-f]{32}$/.test(saved) ? saved : null) ?? memoryKeys[identity] ?? newKey();
  memoryKeys[identity] = key;
  savePendingKeys({...stored, [identity]: key});
  return key;
}
function settleKey(identity) {
  delete memoryKeys[identity];
  const stored = pendingKeys();
  delete stored[identity];
  savePendingKeys(stored);
}

/** True only when the server definitely did not apply the request. */
function definitelyRejected(error) {
  return Boolean(error.status) && error.status >= 400 && error.status < 500 && ![408, 425, 429].includes(error.status);
}

const submitting = new WeakSet();
async function submitCommand(form) {
  if (submitting.has(form)) return;
  submitting.add(form);
  const [method, path] = form.dataset.command.split(' ');
  const status = form.querySelector('.command-status');
  // A new preview always replaces the previous plan, even if it then fails.
  if (form.classList.contains('plan')) form.querySelector('.plan-result')?.remove();
  const button = form.querySelector('button[type="submit"]');
  let identity;
  try {
    const file = form.dataset.body === 'file' ? form.querySelector('input[type="file"]')?.files?.[0] : undefined;
    if (form.dataset.body === 'file' && !file) throw Object.assign(new Error('Choose a file first.'), {status: 400});
    // A precomputed body (data-body="json") is sent exactly as the server rendered it.
    const fixed = form.dataset.body === 'json' ? form.querySelector('[data-json-body]')?.value : undefined;
    const body = file ?? (fixed === undefined ? bodyOf(form) : JSON.parse(fixed));
    // A file's identity is its name, size and modification time, not its bytes.
    const described = file ? {file: file.name, size: file.size, modified: file.lastModified} : body ?? null;
    identity = `${form.dataset.command} ${form.dataset.ifMatch ?? ''} ${JSON.stringify(described)}`;
    const headers = {};
    if (form.dataset.idempotent === 'true') headers['Idempotency-Key'] = keyFor(identity);
    if (form.dataset.ifMatch) headers['If-Match'] = form.dataset.ifMatch;
    button?.setAttribute('disabled', '');
    if (status) status.textContent = 'Working…';
    const result = await call(method, path, body, headers);
    settleKey(identity);
    // Pairing keeps the button disabled for the whole flow (one flow at a time).
    if (form.classList.contains('pairing')) return await pair(form, result);
    if (form.classList.contains('plan')) return showPlan(form, result, body);
    location.reload();
  } catch (error) {
    // Only a definite rejection ends this request's identity; 5xx, 408/429
    // and network failures may have been applied, so a retry reuses the key.
    if (definitelyRejected(error)) settleKey(identity);
    if (status) {
      status.textContent = message(error);
      status.setAttribute('role', 'alert');
    }
  } finally {
    submitting.delete(form);
    button?.removeAttribute('disabled');
  }
}

/**
 * Show a previewed change plan and offer to commit exactly that plan. The plan
 * token stays in this closure; committing it reuses one idempotency key until
 * an outcome is known, and the server rejects an expired or foreign plan.
 */
function showPlan(form, plan, request = {}) {
  const status = form.querySelector('.command-status');
  const commit = form.querySelector('[data-plan-commit]')?.dataset.planCommit;
  form.querySelector('.plan-result')?.remove();
  const box = document.createElement('div');
  box.className = 'plan-result';
  const summary = document.createElement('p');
  const count = plan.affected_ids?.length ?? 0;
  // Name what the plan applies to: the request it was previewed for.
  const target = request.new_root_path ? `Move to ${request.new_root_path}: ` : '';
  summary.textContent = `${target}${count} ${count === 1 ? 'item' : 'items'} affected. This plan expires at ${new Date(plan.expires_at).toLocaleTimeString()}.`;
  box.append(summary);
  if (plan.warnings?.length) {
    const list = document.createElement('ul');
    for (const warning of plan.warnings) {
      const item = document.createElement('li');
      item.textContent = warning;
      list.append(item);
    }
    box.append(list);
  }
  const confirm = document.createElement('button');
  confirm.type = 'button';
  confirm.textContent = 'Apply this change';
  box.append(confirm);
  form.append(box);
  if (status) status.textContent = 'Review the result, then apply it.';
  const [method, path] = (commit ?? '').split(' ');
  const identity = `${commit} ${plan.plan_token}`;
  confirm.addEventListener('click', async () => {
    confirm.setAttribute('disabled', '');
    if (status) status.textContent = 'Working…';
    try {
      await call(method, path, {plan_token: plan.plan_token}, {'Idempotency-Key': keyFor(identity)});
      settleKey(identity);
      location.reload();
    } catch (error) {
      if (definitelyRejected(error)) settleKey(identity);
      if (status) {
        status.textContent = message(error);
        status.setAttribute('role', 'alert');
      }
      confirm.removeAttribute('disabled');
    }
  });
}

/** Show the user code, poll the claim, then exchange the credential for a session. */
async function pair(form, pairing) {
  const status = form.querySelector('.command-status');
  status.textContent = `Approve this code from an administrator device: ${pairing.user_code}`;
  const deadline = Date.parse(pairing.expires_at);
  // The device code and any claimed credential stay in this closure only.
  let credential = null;
  while (Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, pairing.poll_interval_seconds * 1000));
    try {
      // A lost claim response is recovered by claiming again with the same code.
      credential ??= await call('POST', `/api/v2/auth/pairings/${encodeURIComponent(pairing.id)}/claim`, {device_code: pairing.device_code});
      await call('POST', '/api/v2/auth/session', {kind: 'credential', credential: credential.access_token});
      location.reload();
      return;
    } catch (error) {
      // Not approved yet, polling too fast, or no response: keep trying until expiry.
      const transient = !error.status || error.status === 409 || error.status === 429 || error.status >= 500;
      if (!transient) {
        status.textContent = message(error);
        status.setAttribute('role', 'alert');
        return;
      }
    }
  }
  status.textContent = 'The pairing code expired before it was approved.';
  status.setAttribute('role', 'alert');
}

// Editing a previewed form withdraws its plan: it no longer describes the fields.
document.addEventListener('input', event => {
  event.target?.closest?.('form.plan')?.querySelector('.plan-result')?.remove();
});

document.addEventListener('submit', event => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement) || !form.dataset.command) return;
  event.preventDefault();
  void submitCommand(form);
});

// The profile is chosen among those the server rendered for this principal;
// the server validates the cookie against the principal on every request.
document.addEventListener('change', event => {
  const select = event.target;
  if (!(select instanceof HTMLSelectElement) || !select.dataset.profileSwitch) return;
  document.cookie = `${select.dataset.profileSwitch}=${encodeURIComponent(select.value)}; Path=/; SameSite=Strict`;
  location.reload();
});

// A back link uses history only when the entry right before this one is a
// Motion page (the Navigation API lists only this origin's contiguous
// entries), so it never leaves the app; otherwise it follows its href.
function previousEntryIsMotion() {
  const index = globalThis.navigation?.currentEntry?.index ?? -1;
  if (index < 1) return false;
  const previous = navigation.entries()[index - 1]?.url;
  return Boolean(previous) && new URL(previous).origin === location.origin;
}
document.addEventListener('click', event => {
  const link = event.target?.closest?.('a[data-back="history"]');
  if (!link || event.defaultPrevented || event.button !== 0
    || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || !previousEntryIsMotion()) return;
  event.preventDefault();
  history.back();
});

// Motion UI bridge: the one small external module on every server-rendered
// page. It holds no catalog or viewing state and never evaluates strings as
// code. It does three things:
//   1. submits command forms (<form data-command="METHOD /api/v2/...">) to the
//      public API with CSRF, a stable idempotency key and If-Match, then
//      reloads the server-rendered page;
//   2. runs browser pairing from the sign-in page;
//   3. switches the viewing profile among those the server rendered.

const csrf = () => document.querySelector('meta[name="motion-csrf"]')?.content ?? '';
const newKey = () => crypto.randomUUID().replaceAll('-', '');

// Move focus to an error panel so keyboard and screen-reader users notice it.
const alert = document.querySelector('main > [role="alert"], main [role="alert"]');
if (alert) {
  alert.setAttribute('tabindex', '-1');
  alert.focus({preventScroll: true});
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
    switch (type) {
      case 'bool': body[name] = input.checked; break;
      case 'number': body[name] = input.value === '' ? null : Number(input.value); break;
      case 'list': body[name] = input.value.split(',').map(v => v.trim()).filter(Boolean); break;
      case 'json': body[name] = JSON.parse(input.value); break;
      case 'member':
        body[name] ??= [];
        if (input.checked) body[name].push(input.value);
        break;
      default: body[name] = input.value;
    }
  }
  return any ? body : undefined;
}

/** One public API call; returns parsed JSON or throws an Error carrying the Problem. */
async function call(method, path, body, headers = {}) {
  const response = await fetch(path, {
    method, credentials: 'same-origin', redirect: 'error', cache: 'no-store',
    headers: {'Accept': 'application/json', 'X-CSRF-Token': csrf(), ...(body === undefined ? {} : {'Content-Type': 'application/json'}), ...headers},
    body: body === undefined ? undefined : JSON.stringify(body),
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
  const button = form.querySelector('button[type="submit"]');
  let identity;
  try {
    const body = bodyOf(form);
    identity = `${form.dataset.command} ${form.dataset.ifMatch ?? ''} ${JSON.stringify(body ?? null)}`;
    const headers = {};
    if (form.dataset.idempotent === 'true') headers['Idempotency-Key'] = keyFor(identity);
    if (form.dataset.ifMatch) headers['If-Match'] = form.dataset.ifMatch;
    button?.setAttribute('disabled', '');
    if (status) status.textContent = 'Working…';
    const result = await call(method, path, body, headers);
    settleKey(identity);
    // Pairing keeps the button disabled for the whole flow (one flow at a time).
    if (form.classList.contains('pairing')) return await pair(form, result);
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

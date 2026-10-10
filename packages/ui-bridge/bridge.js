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

// Idempotency keys survive retries of the same body until an outcome is known.
const pendingKeys = new WeakMap();

async function submitCommand(form) {
  const [method, path] = form.dataset.command.split(' ');
  const status = form.querySelector('.command-status');
  const button = form.querySelector('button[type="submit"]');
  const body = bodyOf(form);
  const signature = JSON.stringify(body ?? null);
  const headers = {};
  if (form.dataset.idempotent === 'true') {
    const saved = pendingKeys.get(form);
    const key = saved?.signature === signature ? saved.key : newKey();
    pendingKeys.set(form, {signature, key});
    headers['Idempotency-Key'] = key;
  }
  if (form.dataset.ifMatch) headers['If-Match'] = form.dataset.ifMatch;
  button?.setAttribute('disabled', '');
  if (status) status.textContent = 'Working…';
  try {
    const result = await call(method, path, body, headers);
    pendingKeys.delete(form);
    if (form.classList.contains('pairing')) return pair(form, result);
    location.reload();
  } catch (error) {
    // A known rejection ends this attempt; an unknown outcome keeps the key.
    if (error.status) pendingKeys.delete(form);
    if (status) {
      status.textContent = message(error);
      status.setAttribute('role', 'alert');
    }
  } finally {
    button?.removeAttribute('disabled');
  }
}

/** Show the user code, poll the claim, then exchange the credential for a session. */
async function pair(form, pairing) {
  const status = form.querySelector('.command-status');
  status.textContent = `Approve this code from an administrator device: ${pairing.user_code}`;
  const deadline = Date.parse(pairing.expires_at);
  while (Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, pairing.poll_interval_seconds * 1000));
    try {
      const credential = await call('POST', `/api/v2/auth/pairings/${encodeURIComponent(pairing.id)}/claim`, {device_code: pairing.device_code});
      await call('POST', '/api/v2/auth/session', {kind: 'credential', credential: credential.access_token});
      location.reload();
      return;
    } catch (error) {
      // 409: not approved yet; 429: polling too fast. Anything else ends it.
      if (error.status !== 409 && error.status !== 429) {
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

const form = document.getElementById('connect');
const status = document.getElementById('status');
const show = (text, error = false) => { status.textContent = text; status.setAttribute('role', error ? 'alert' : 'status'); };
form.addEventListener('submit', async event => {
  event.preventDefault();
  const button = form.querySelector('button[type=submit]');
  button.disabled = true;
  show('Verifying server…');
  const credential = document.getElementById('credential');
  const request = {origin: document.getElementById('origin').value, serverId: document.getElementById('server').value,
    credential: credential.value, remember: document.getElementById('remember').checked};
  credential.value = '';
  try {
    const result = await window.motionHost.connect(request);
    show(`Connected to ${result.serverId}. Closing this window leaves the server running.`);
  } catch (error) { show(error.message, true); }
  finally { button.disabled = false; }
});
document.getElementById('disconnect').addEventListener('click', async event => {
  const button = event.currentTarget;
  button.disabled = true;
  try { await window.motionHost.disconnect(); show('Disconnected. The server is still running.'); }
  catch (error) { show(error.message, true); }
  finally { button.disabled = false; }
});

window.motionHost.onStatus(message => show(message, true));

for (const [id, operation, message] of [
  ['start-local', 'startLocal', 'Connected to the local server owned by this desktop. Quitting stops this server.'],
  ['stop-local', 'stopLocal', 'The desktop-owned server has stopped.'],
]) document.getElementById(id).addEventListener('click', async event => {
  event.target.disabled = true;
  try { await window.motionHost[operation](); show(message); }
  catch (error) { show(error.message, true); }
  finally { event.target.disabled = false; }
});

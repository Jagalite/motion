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
document.getElementById('disconnect').addEventListener('click', async () => {
  await window.motionHost.disconnect(); show('Disconnected. The server is still running.');
});

window.motionHost.onStatus(message => show(message, true));

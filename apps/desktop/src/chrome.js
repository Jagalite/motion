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

// Several servers or profiles may have saved titles here: offer a choice
// (labels are built with textContent, never markup).
const rootChoice = document.getElementById('offline-root');
async function listOfflineRoots() {
  try {
    const roots = await window.motionHost.offlineRoots();
    rootChoice.replaceChildren(...roots.map(root => Object.assign(document.createElement('option'),
      {value: root.name, selected: root.current, textContent: `${root.label} (${root.titles} saved)`})));
    document.getElementById('offline-root-label').hidden = roots.length < 2;
  } catch { /* No choice offered: the most recently synchronized library opens. */ }
}
void listOfflineRoots();

for (const [id, operation, message] of [
  ['start-local', 'startLocal', 'Connected to the local server owned by this desktop. Quitting stops this server.'],
  ['stop-local', 'stopLocal', 'The desktop-owned server has stopped.'],
  ['downloads', 'openDownloads', 'Offline downloads are open. Progress is saved on this device.'],
]) document.getElementById(id).addEventListener('click', async event => {
  event.target.disabled = true;
  const argument = id === 'downloads' && !document.getElementById('offline-root-label').hidden ? rootChoice.value : undefined;
  try { await window.motionHost[operation](argument); show(message); if (id === 'downloads') void listOfflineRoots(); }
  catch (error) { show(error.message, true); }
  finally { event.target.disabled = false; }
});

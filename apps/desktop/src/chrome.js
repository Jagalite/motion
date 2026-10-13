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
// (labels are built with textContent, never markup). The list is re-read
// whenever it may be used, so libraries saved or removed by the background
// download agent appear or disappear; a selection survives only while its
// root still exists.
const rootChoice = document.getElementById('offline-root');
const rootLabel = document.getElementById('offline-root-label');
async function listOfflineRoots() {
  try {
    const roots = await window.motionHost.offlineRoots();
    const chosen = roots.some(root => root.name === rootChoice.value) ? rootChoice.value : null;
    rootChoice.replaceChildren(...roots.map(root => Object.assign(document.createElement('option'),
      {value: root.name, selected: chosen ? root.name === chosen : root.current, textContent: `${root.label} (${root.titles} saved)`})));
    rootLabel.hidden = roots.length < 2;
  } catch { rootLabel.hidden = true; /* No choice offered: the most recently synchronized library opens. */ }
}
void listOfflineRoots();
rootChoice.addEventListener('focus', () => void listOfflineRoots());

for (const [id, operation, message] of [
  ['start-local', 'startLocal', 'Connected to the local server owned by this desktop. Quitting stops this server.'],
  ['stop-local', 'stopLocal', 'The desktop-owned server has stopped.'],
  ['downloads', 'openDownloads', 'Offline downloads are open. Progress is saved on this device.'],
]) document.getElementById(id).addEventListener('click', async event => {
  const button = event.currentTarget;
  button.disabled = true;
  try {
    if (id === 'downloads') await listOfflineRoots();
    // A listed root is named even when it is the only one (the chooser is
    // hidden): the most recently synchronized root may since have been emptied.
    await window.motionHost[operation](id === 'downloads' && rootChoice.value ? rootChoice.value : undefined);
    show(message);
  } catch (error) { show(error.message, true); }
  finally {
    button.disabled = false;
    if (id === 'downloads') void listOfflineRoots();
  }
});

const {contextBridge, ipcRenderer} = require('electron');
contextBridge.exposeInMainWorld('motionHost', Object.freeze({
  startLocal: () => ipcRenderer.invoke('motion:start-local'),
  stopLocal: () => ipcRenderer.invoke('motion:stop-local'),
  openDownloads: root => ipcRenderer.invoke('motion:downloads', typeof root === 'string' ? root : null),
  offlineRoots: () => ipcRenderer.invoke('motion:offline-roots'),
  offlineRestore: () => ipcRenderer.invoke('motion:offline-restore'),
  chooseOfflineRestore: (root, choice) => ipcRenderer.invoke('motion:offline-restore-choose', String(root), String(choice)),
  connect: request => ipcRenderer.invoke('motion:connect', request),
  disconnect: () => ipcRenderer.invoke('motion:disconnect'),
  onStatus: callback => {
    const handler = (_event, message) => callback(String(message));
    ipcRenderer.on('motion:status', handler);
    return () => ipcRenderer.removeListener('motion:status', handler);
  },
}));

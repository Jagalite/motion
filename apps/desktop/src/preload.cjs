const {contextBridge, ipcRenderer} = require('electron');
contextBridge.exposeInMainWorld('motionHost', Object.freeze({
  startLocal: () => ipcRenderer.invoke('motion:start-local'),
  stopLocal: () => ipcRenderer.invoke('motion:stop-local'),
  openDownloads: () => ipcRenderer.invoke('motion:downloads'),
  connect: request => ipcRenderer.invoke('motion:connect', request),
  disconnect: () => ipcRenderer.invoke('motion:disconnect'),
  onStatus: callback => {
    const handler = (_event, message) => callback(String(message));
    ipcRenderer.on('motion:status', handler);
    return () => ipcRenderer.removeListener('motion:status', handler);
  },
}));

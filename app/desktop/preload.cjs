// Main-window preload (sandboxed, so CommonJS). Bridges: chrome and zoom (the native window controls follow the app's
// palette and mode), clipboard writes (navigator.clipboard rejects when the document is not focused) and dropped-file paths.
const { contextBridge, ipcRenderer, webUtils } = require("electron");
contextBridge.exposeInMainWorld("iaDesktop", {
  setChrome: (c) => ipcRenderer.send("ia:chrome", c),
  setZoom: (z) => ipcRenderer.send("ia:zoom", z),
  copyText: (t) => ipcRenderer.send("ia:copy", t),
  getPathForFile: (file) => webUtils.getPathForFile(file),
});

/**
 * Centinelo Phone — preload. Minimal, typed-ish bridge between the sandboxed
 * renderer (SIP engine + UI) and the main process (store, tray, notifications,
 * hotkeys, protocol/bridge dial requests).
 */
'use strict';

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('centinelo', {
  // Settings / history stores (live in main)
  getSettings: () => ipcRenderer.invoke('settings:get'),
  saveSettings: (next) => ipcRenderer.invoke('settings:save', next),
  getHistory: () => ipcRenderer.invoke('history:get'),
  addHistory: (entry) => ipcRenderer.invoke('history:add', entry),
  setLastHistoryDuration: (sec) => ipcRenderer.invoke('history:set-duration', sec),

  // Patient lookup (main-process fetch, no CORS)
  lookup: (number) => ipcRenderer.invoke('lookup', number),

  // OS integration
  notify: (payload) => ipcRenderer.send('notify', payload),
  reportCallState: (state) => ipcRenderer.send('call-state', state),
  openExternal: (url) => ipcRenderer.invoke('open-external', url),
  openSettings: () => ipcRenderer.invoke('open-settings'),
  showWindow: () => ipcRenderer.invoke('show-window'),
  appVersion: () => ipcRenderer.invoke('app-version'),

  // Events pushed from main
  onDialRequest: (fn) => ipcRenderer.on('dial-request', (e, number) => fn(number)),
  onHotkey: (fn) => ipcRenderer.on('hotkey', (e, action) => fn(action)),
  onSettingsChanged: (fn) => ipcRenderer.on('settings-changed', (e, s) => fn(s)),
  onHistoryChanged: (fn) => ipcRenderer.on('history-changed', (e, h) => fn(h))
});

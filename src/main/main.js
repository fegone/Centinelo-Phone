/**
 * Centinelo Phone — Electron main process.
 *
 * Owns: app lifecycle, tray, windows, global hotkeys, settings/history store,
 * OS notifications, centinelo:// + optional tel: protocol, the localhost
 * click-to-call bridge, and the patient-lookup proxy (main-process fetch =
 * no CORS). The renderer owns ALL WebRTC/audio (SIP engine) — the main window
 * is never destroyed while the app runs (close = hide to tray) so calls
 * survive any UI interaction.
 *
 * NO PHI IN LOGS unless settings.debug is explicitly enabled.
 */
'use strict';

const { app, BrowserWindow, Tray, Menu, ipcMain, globalShortcut, Notification, clipboard, shell, session, nativeImage } = require('electron');
const path = require('path');
const fs = require('fs');
const http = require('http');
const crypto = require('crypto');

// ---------------------------------------------------------------------------
// Single instance — a second launch just focuses the existing window and
// forwards any centinelo://dial?number=... argument.
// ---------------------------------------------------------------------------
const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
}

let mainWindow = null;
let settingsWindow = null;
let tray = null;
let isQuitting = false;
let missedCount = 0;
let bridgeServer = null;
let currentCallState = 'disconnected';

// ---------------------------------------------------------------------------
// Tiny JSON settings/history store (zero-dep; electron-store v10 is ESM-only).
// ---------------------------------------------------------------------------
const DEFAULT_SETTINGS = {
  wssUrl: '', // e.g. wss://pbx.example.com:8089/ws
  extension: '',
  password: '',
  displayName: '',
  lookupUrl: '', // optional caller-name lookup endpoint
  lookupToken: '',
  newPatientUrl: '', // optional deep link to your PMS/EHR new-patient screen
  iceServers: [],
  micDeviceId: 'default',
  speakerDeviceId: 'default',
  ringerDeviceId: 'default',
  favorites: [], // up to 4 { ext, label } BLF lamps
  // --- new in Centinelo ---
  dnd: false,
  autoAnswer: false,
  autoAnswerDelayMs: 2000,
  callWaiting: true,
  echoCancellation: true,
  noiseSuppression: true,
  autoGainControl: true,
  globalHotkeys: true,
  hotkeyAnswer: 'Control+Shift+A',
  hotkeyHangup: 'Control+Shift+X',
  hotkeyDialClipboard: 'Control+Shift+D',
  registerTelHandler: false,
  clickToCallBridge: true,
  clickToCallPort: 38911,
  clickToCallToken: '',
  alwaysOnTop: false,
  startOnBoot: true,
  startMinimized: true,
  // Cert pinning for a private/internal CA (WSS). Primary path is installing
  // your CA cert in the OS trust store; this pin is the fallback so the app
  // works even before the CA is installed. sha256 hex of the LEAF cert DER.
  pinnedCertSha256: [],
  debug: false
};

const storePath = () => path.join(app.getPath('userData'), 'settings.json');
const historyPath = () => path.join(app.getPath('userData'), 'history.json');

function readJson(file, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (e) {
    return fallback;
  }
}

function writeJson(file, data) {
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify(data, null, 2), 'utf8');
  } catch (e) {
    console.error('[Centinelo] store-write-failed', e.message);
  }
}

function getSettings() {
  const stored = readJson(storePath(), {});
  const merged = { ...DEFAULT_SETTINGS, ...stored };
  // First run: mint a random token for the click-to-call bridge.
  if (!merged.clickToCallToken) {
    merged.clickToCallToken = crypto.randomBytes(16).toString('hex');
    writeJson(storePath(), merged);
  }
  return merged;
}

function saveSettings(next) {
  const merged = { ...getSettings(), ...next };
  writeJson(storePath(), merged);
  return merged;
}

const MAX_HISTORY = 50;
const HISTORY_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000; // 30 days

function getHistory() {
  const cutoff = Date.now() - HISTORY_MAX_AGE_MS;
  return readJson(historyPath(), []).filter((e) => e && e.time > cutoff).slice(0, MAX_HISTORY);
}

function addHistory(entry) {
  const history = [entry, ...getHistory()].slice(0, MAX_HISTORY);
  writeJson(historyPath(), history);
  broadcast('history-changed', history);
  return history;
}

function setLastHistoryDuration(durationSec) {
  const history = getHistory();
  if (history.length) {
    history[0].duration = durationSec;
    writeJson(historyPath(), history);
    broadcast('history-changed', history);
  }
  return history;
}

function broadcast(channel, payload) {
  for (const win of BrowserWindow.getAllWindows()) {
    if (!win.isDestroyed()) win.webContents.send(channel, payload);
  }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
function createMainWindow() {
  const settings = getSettings();
  mainWindow = new BrowserWindow({
    width: 300,
    height: 520,
    minWidth: 280,
    minHeight: 440,
    show: !settings.startMinimized,
    alwaysOnTop: !!settings.alwaysOnTop,
    resizable: true,
    maximizable: false,
    fullscreenable: false,
    autoHideMenuBar: true,
    title: 'Centinelo Phone',
    icon: appIcon(),
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      backgroundThrottling: false // SIP timers must never throttle
    }
  });

  mainWindow.loadFile(path.join(__dirname, '..', 'renderer', 'index.html'));

  // Close = hide to tray. The engine (and any live call) keeps running.
  mainWindow.on('close', (e) => {
    if (!isQuitting) {
      e.preventDefault();
      mainWindow.hide();
    }
  });

  mainWindow.on('show', () => {
    missedCount = 0;
    updateTray();
    if (process.platform === 'win32') mainWindow.setOverlayIcon(null, '');
  });
}

function createSettingsWindow() {
  if (settingsWindow && !settingsWindow.isDestroyed()) {
    settingsWindow.focus();
    return;
  }
  settingsWindow = new BrowserWindow({
    width: 560,
    height: 720,
    autoHideMenuBar: true,
    title: 'Centinelo Phone — Settings',
    icon: appIcon(),
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false
    }
  });
  settingsWindow.loadFile(path.join(__dirname, '..', 'renderer', 'settings.html'));
  settingsWindow.on('closed', () => { settingsWindow = null; });
}

function appIcon() {
  const p = path.join(__dirname, '..', '..', 'build', 'icons-src', 'icon-128.png');
  return fs.existsSync(p) ? nativeImage.createFromPath(p) : undefined;
}

function showMainWindow() {
  if (!mainWindow) return;
  if (mainWindow.isMinimized()) mainWindow.restore();
  mainWindow.show();
  mainWindow.focus();
}

// ---------------------------------------------------------------------------
// Tray
// ---------------------------------------------------------------------------
function updateTray() {
  if (!tray) return;
  const settings = getSettings();
  const stateLabel = {
    'disconnected': 'Disconnected',
    'connecting': 'Connecting…',
    'registered': 'Ready',
    'ringing': '☎ Incoming call…',
    'calling': 'Calling…',
    'in-call': 'On a call',
    'held': 'Call on hold'
  }[currentCallState] || currentCallState;

  tray.setToolTip(`Centinelo Phone — ${stateLabel}${missedCount ? ` · ${missedCount} missed` : ''}`);
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: 'Open Centinelo Phone', click: showMainWindow },
    { type: 'separator' },
    {
      label: 'Do Not Disturb',
      type: 'checkbox',
      checked: !!settings.dnd,
      click: (item) => {
        saveSettings({ dnd: item.checked });
        broadcast('settings-changed', getSettings());
        updateTray();
      }
    },
    {
      label: 'Always on top',
      type: 'checkbox',
      checked: !!settings.alwaysOnTop,
      click: (item) => {
        saveSettings({ alwaysOnTop: item.checked });
        if (mainWindow) mainWindow.setAlwaysOnTop(item.checked);
      }
    },
    { type: 'separator' },
    { label: 'Settings…', click: createSettingsWindow },
    { type: 'separator' },
    { label: 'Quit', click: () => { isQuitting = true; app.quit(); } }
  ]));
}

function createTray() {
  const icon = appIcon();
  tray = new Tray(icon || nativeImage.createEmpty());
  tray.on('click', showMainWindow);
  tray.on('double-click', showMainWindow);
  updateTray();
}

// ---------------------------------------------------------------------------
// Global hotkeys
// ---------------------------------------------------------------------------
function registerHotkeys() {
  globalShortcut.unregisterAll();
  const s = getSettings();
  if (!s.globalHotkeys) return;
  const tryRegister = (accel, fn) => {
    if (!accel) return;
    try {
      globalShortcut.register(accel, fn);
    } catch (e) {
      console.error('[Centinelo] hotkey-register-failed', accel);
    }
  };
  tryRegister(s.hotkeyAnswer, () => broadcast('hotkey', 'answer'));
  tryRegister(s.hotkeyHangup, () => broadcast('hotkey', 'hangup'));
  tryRegister(s.hotkeyDialClipboard, () => {
    const text = (clipboard.readText() || '').trim();
    const digits = text.replace(/[^\d+*#]/g, '');
    if (digits.length >= 2) {
      broadcast('dial-request', digits);
      showMainWindow();
    }
  });
}

// ---------------------------------------------------------------------------
// centinelo:// + tel: protocol handling
// ---------------------------------------------------------------------------
function extractDialTarget(rawUrl) {
  try {
    if (rawUrl.startsWith('tel:')) {
      return decodeURIComponent(rawUrl.slice(4)).replace(/[^\d+*#]/g, '');
    }
    const u = new URL(rawUrl);
    if (u.protocol === 'centinelo:') {
      const num = u.searchParams.get('number') || u.hostname || u.pathname.replace(/^\/+/, '');
      return decodeURIComponent(num || '').replace(/[^\d+*#]/g, '');
    }
  } catch (e) { /* malformed — ignore */ }
  return '';
}

function handleProtocolArgv(argv) {
  const link = argv.find((a) => a.startsWith('centinelo://') || a.startsWith('tel:'));
  if (link) {
    const number = extractDialTarget(link);
    if (number) {
      broadcast('dial-request', number);
      showMainWindow();
    }
  }
}

// ---------------------------------------------------------------------------
// Click-to-call localhost bridge (for the companion Chrome extension).
// 127.0.0.1 only + shared token. POST /dial {number} · GET /ping.
// ---------------------------------------------------------------------------
function startBridge() {
  const s = getSettings();
  if (bridgeServer) {
    try { bridgeServer.close(); } catch (e) { /* noop */ }
    bridgeServer = null;
  }
  if (!s.clickToCallBridge) return;

  bridgeServer = http.createServer((req, res) => {
    res.setHeader('Access-Control-Allow-Origin', '*'); // localhost-only listener; extension posts from page origins
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type, X-Centinelo-Token');
    res.setHeader('Access-Control-Allow-Methods', 'POST, GET, OPTIONS');
    res.setHeader('Access-Control-Allow-Private-Network', 'true'); // Chrome PNA preflight
    if (req.method === 'OPTIONS') { res.writeHead(204); res.end(); return; }

    const token = req.headers['x-centinelo-token'] || '';
    if (token !== s.clickToCallToken) {
      res.writeHead(403, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'bad token' }));
      return;
    }

    if (req.method === 'GET' && req.url === '/ping') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ app: 'centinelo-phone', state: currentCallState }));
      return;
    }

    if (req.method === 'POST' && req.url === '/dial') {
      let body = '';
      req.on('data', (c) => { body += c; if (body.length > 4096) req.destroy(); });
      req.on('end', () => {
        try {
          const { number } = JSON.parse(body || '{}');
          const clean = String(number || '').replace(/[^\d+*#]/g, '');
          if (clean.length >= 2) {
            broadcast('dial-request', clean);
            showMainWindow();
            res.writeHead(200, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify({ ok: true }));
            return;
          }
        } catch (e) { /* fallthrough */ }
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'bad request' }));
      });
      return;
    }

    res.writeHead(404);
    res.end();
  });
  bridgeServer.on('error', (e) => console.error('[Centinelo] bridge-error', e.message));
  bridgeServer.listen(s.clickToCallPort, '127.0.0.1');
}

// ---------------------------------------------------------------------------
// Patient lookup proxy — main-process fetch (no CORS), degrades to null.
// ---------------------------------------------------------------------------
async function lookupPatient(number) {
  const s = getSettings();
  if (!s.lookupUrl) return null;
  try {
    const u = new URL(s.lookupUrl);
    u.searchParams.set('num', number);
    const resp = await fetch(u.toString(), {
      headers: s.lookupToken ? { 'X-Token': s.lookupToken } : {},
      signal: AbortSignal.timeout(4000)
    });
    if (!resp.ok) return null;
    const data = await resp.json();
    return data && data.name ? { name: data.name, profileId: data.profile_id || null } : null;
  } catch (e) {
    return null; // lookup is best-effort
  }
}

// ---------------------------------------------------------------------------
// Certificate handling for a private/internal CA (WSS to the PBX).
// Primary path: install the CA cert in the OS trust store.
// Fallback: settings.pinnedCertSha256 — accept ONLY the pinned leaf cert and
// ONLY for the configured WSS host (sha256 hex of the leaf cert DER).
//
// GOTCHA (verified live 2026-07-15): app.on('certificate-error') does NOT
// fire for WebSocket connections — WSS with an untrusted CA dies with
// net_error -202 before any interceptable event. setCertificateVerifyProc
// on the session is the ONLY hook that covers WSS.
// ---------------------------------------------------------------------------
function pemToDerSha256(pem) {
  try {
    const b64 = String(pem).replace(/-----(BEGIN|END) CERTIFICATE-----/g, '').replace(/\s+/g, '');
    if (!b64) return '';
    return crypto.createHash('sha256').update(Buffer.from(b64, 'base64')).digest('hex');
  } catch (e) {
    return '';
  }
}

function pinMatches(certificate) {
  const s = getSettings();
  if (!Array.isArray(s.pinnedCertSha256) || !s.pinnedCertSha256.length) return false;
  const fp = certificate && certificate.data ? pemToDerSha256(certificate.data) : '';
  if (!fp) return false;
  return s.pinnedCertSha256
    .map((x) => String(x).toLowerCase().replace(/[^a-f0-9]/g, ''))
    .includes(fp);
}

function wssHostname() {
  try { return new URL(getSettings().wssUrl).hostname; } catch (e) { return ''; }
}

function installCertVerifyProc() {
  session.defaultSession.setCertificateVerifyProc((request, callback) => {
    const { hostname, certificate, verificationResult } = request;
    if (verificationResult === 'net::OK') { callback(-3); return; } // chromium says OK
    if (hostname && hostname === wssHostname() && pinMatches(certificate)) {
      callback(0); // pinned internal-CA leaf for the PBX host only
      return;
    }
    callback(-3); // defer to chromium's (failing) verdict for everything else
  });
}

// Non-WebSocket surfaces (page loads/fetch) still emit certificate-error.
app.on('certificate-error', (event, webContents, url, error, certificate, callback) => {
  let urlHost = '';
  try { urlHost = new URL(url).hostname; } catch (e) { /* noop */ }
  if (urlHost && urlHost === wssHostname() && pinMatches(certificate)) {
    event.preventDefault();
    callback(true);
    return;
  }
  callback(false);
});

// ---------------------------------------------------------------------------
// IPC surface (preload → renderer)
// ---------------------------------------------------------------------------
ipcMain.handle('settings:get', () => getSettings());
ipcMain.handle('settings:save', (e, next) => {
  const merged = saveSettings(next || {});
  broadcast('settings-changed', merged);
  registerHotkeys();
  startBridge();
  applyLoginItem();
  if (mainWindow) mainWindow.setAlwaysOnTop(!!merged.alwaysOnTop);
  updateTray();
  return merged;
});
ipcMain.handle('history:get', () => getHistory());
ipcMain.handle('history:add', (e, entry) => addHistory(entry));
ipcMain.handle('history:set-duration', (e, sec) => setLastHistoryDuration(sec));
ipcMain.handle('lookup', (e, number) => lookupPatient(number));
ipcMain.handle('open-external', (e, url) => {
  if (/^https?:\/\//.test(url)) shell.openExternal(url);
});
ipcMain.handle('open-settings', () => createSettingsWindow());
ipcMain.handle('show-window', () => showMainWindow());
ipcMain.handle('app-version', () => app.getVersion());

ipcMain.on('call-state', (e, state) => {
  currentCallState = state && state.state ? state.state : 'disconnected';
  updateTray();
  if (currentCallState === 'ringing' && mainWindow && !mainWindow.isVisible()) {
    showMainWindow(); // incoming call always surfaces the phone
  }
  if (currentCallState === 'ringing' && mainWindow) {
    mainWindow.flashFrame(true);
  }
});

ipcMain.on('notify', (e, { title, body, kind }) => {
  const s = getSettings();
  if (!Notification.isSupported()) return;
  const n = new Notification({
    title: title || 'Centinelo Phone',
    body: body || '',
    icon: appIcon(),
    silent: kind === 'incoming' // engine plays its own ringtone
  });
  n.on('click', showMainWindow);
  n.show();
  if (kind === 'missed') {
    missedCount += 1;
    updateTray();
    if (process.platform === 'win32' && mainWindow) {
      const badge = appIcon();
      if (badge) mainWindow.setOverlayIcon(badge, `${missedCount} missed`);
    }
  }
});

// ---------------------------------------------------------------------------
// Autostart
// ---------------------------------------------------------------------------
function applyLoginItem() {
  const s = getSettings();
  if (process.platform === 'win32' || process.platform === 'darwin') {
    app.setLoginItemSettings({
      openAtLogin: !!s.startOnBoot,
      args: ['--hidden']
    });
  }
}

// ---------------------------------------------------------------------------
// App lifecycle
// ---------------------------------------------------------------------------
app.on('second-instance', (e, argv) => {
  handleProtocolArgv(argv);
  showMainWindow();
});

app.on('open-url', (e, url) => { // macOS protocol
  e.preventDefault();
  const number = extractDialTarget(url);
  if (number) {
    broadcast('dial-request', number);
    showMainWindow();
  }
});

app.whenReady().then(() => {
  installCertVerifyProc();

  // Mic permission: grant media requests from our own renderer (file://).
  session.defaultSession.setPermissionRequestHandler((wc, permission, callback) => {
    callback(permission === 'media');
  });

  app.setAsDefaultProtocolClient('centinelo');
  const s = getSettings();
  if (s.registerTelHandler) {
    app.setAsDefaultProtocolClient('tel');
  }

  createMainWindow();
  createTray();
  registerHotkeys();
  startBridge();
  applyLoginItem();
  handleProtocolArgv(process.argv);

  app.on('activate', () => showMainWindow()); // macOS dock
});

app.on('before-quit', () => { isQuitting = true; });
app.on('will-quit', () => globalShortcut.unregisterAll());
app.on('window-all-closed', (e) => {
  // Tray app: never quit on window close (close is intercepted anyway).
  if (process.platform !== 'darwin' && isQuitting) app.quit();
});

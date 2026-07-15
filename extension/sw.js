/**
 * Centinelo Click-to-Call — service worker.
 * Relays dial requests from content scripts to the Centinelo Phone desktop
 * app's localhost bridge (extension SW fetch avoids page mixed-content/PNA).
 */
'use strict';

const DEFAULTS = { port: 38911, token: '' };

async function getConfig() {
  const stored = await chrome.storage.sync.get(DEFAULTS);
  return { ...DEFAULTS, ...stored };
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (!message || message.cmd !== 'dial' || !message.number) return false;
  (async () => {
    const cfg = await getConfig();
    if (!cfg.token) {
      sendResponse({ ok: false, error: 'no-token' });
      return;
    }
    try {
      const resp = await fetch(`http://127.0.0.1:${cfg.port}/dial`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'X-Centinelo-Token': cfg.token
        },
        body: JSON.stringify({ number: message.number })
      });
      sendResponse({ ok: resp.ok, status: resp.status });
    } catch (e) {
      sendResponse({ ok: false, error: 'app-not-running' });
    }
  })();
  return true;
});

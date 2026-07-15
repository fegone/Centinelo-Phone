'use strict';

const DEFAULTS = { port: 38911, token: '' };

async function load() {
  const cfg = await chrome.storage.sync.get(DEFAULTS);
  document.getElementById('port').value = cfg.port;
  document.getElementById('token').value = cfg.token;
}

document.getElementById('save').addEventListener('click', async () => {
  await chrome.storage.sync.set({
    port: Number(document.getElementById('port').value) || 38911,
    token: document.getElementById('token').value.trim()
  });
  const st = document.getElementById('status');
  st.textContent = 'Saved ✓';
  setTimeout(() => { st.textContent = ''; }, 2000);
});

load();

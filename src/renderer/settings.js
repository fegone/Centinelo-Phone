/**
 * Centinelo Phone — settings window logic. Loads/saves via the main-process
 * store; device lists come from enumerateDevices in this renderer.
 */
(function () {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const MAX_FAVORITES = 4;

  const TEXT_FIELDS = [
    'wssUrl', 'extension', 'password', 'displayName',
    'lookupUrl', 'lookupToken', 'newPatientUrl',
    'hotkeyAnswer', 'hotkeyHangup', 'hotkeyDialClipboard'
  ];
  const NUM_FIELDS = ['autoAnswerDelayMs', 'clickToCallPort'];
  const BOOL_FIELDS = [
    'echoCancellation', 'noiseSuppression', 'autoGainControl',
    'callWaiting', 'dnd', 'autoAnswer',
    'globalHotkeys', 'registerTelHandler', 'clickToCallBridge',
    'startOnBoot', 'startMinimized', 'alwaysOnTop', 'debug'
  ];

  function favRows() {
    return [...document.querySelectorAll('#favContainer .fav-row')];
  }

  function buildFavRows(favorites) {
    const container = $('favContainer');
    container.innerHTML = '';
    for (let i = 0; i < MAX_FAVORITES; i++) {
      const fav = (favorites && favorites[i]) || { ext: '', label: '' };
      const row = document.createElement('div');
      row.className = 'fav-row';
      const ext = document.createElement('input');
      ext.type = 'text';
      ext.className = 'ext';
      ext.placeholder = 'Ext';
      ext.value = fav.ext || '';
      const label = document.createElement('input');
      label.type = 'text';
      label.placeholder = 'Label (e.g. Front 2)';
      label.value = fav.label || '';
      row.appendChild(ext);
      row.appendChild(label);
      container.appendChild(row);
    }
  }

  function readFavRows() {
    return favRows()
      .map((row) => {
        const [ext, label] = row.querySelectorAll('input');
        return { ext: ext.value.trim(), label: label.value.trim() };
      })
      .filter((f) => f.ext);
  }

  async function fillDeviceSelects(settings) {
    let devices = [];
    try {
      devices = await navigator.mediaDevices.enumerateDevices();
    } catch (e) { /* device labels may be empty before mic permission */ }

    const fill = (sel, kind, current) => {
      sel.innerHTML = '';
      const def = document.createElement('option');
      def.value = 'default';
      def.textContent = 'System default';
      sel.appendChild(def);
      devices.filter((d) => d.kind === kind).forEach((d) => {
        if (!d.deviceId || d.deviceId === 'default') return;
        const opt = document.createElement('option');
        opt.value = d.deviceId;
        opt.textContent = d.label || `${kind} (${d.deviceId.slice(0, 8)}…)`;
        sel.appendChild(opt);
      });
      sel.value = current || 'default';
      if (sel.value !== (current || 'default')) sel.value = 'default'; // device unplugged
    };

    fill($('micDevice'), 'audioinput', settings.micDeviceId);
    fill($('speakerDevice'), 'audiooutput', settings.speakerDeviceId);
    fill($('ringerDevice'), 'audiooutput', settings.ringerDeviceId);
  }

  async function load() {
    const s = await window.centinelo.getSettings();
    TEXT_FIELDS.forEach((k) => { $(k).value = s[k] || ''; });
    NUM_FIELDS.forEach((k) => { $(k).value = s[k] != null ? s[k] : ''; });
    BOOL_FIELDS.forEach((k) => { $(k).checked = !!s[k]; });
    $('clickToCallToken').value = s.clickToCallToken || '';
    buildFavRows(s.favorites);
    await fillDeviceSelects(s);
    const v = await window.centinelo.appVersion();
    $('versionSub').textContent = `v${v} — SIP over WSS · Opus/G.722/G.711 · BLF · call waiting`;
  }

  async function save() {
    const next = {};
    TEXT_FIELDS.forEach((k) => { next[k] = $(k).value.trim(); });
    NUM_FIELDS.forEach((k) => { next[k] = Number($(k).value) || 0; });
    BOOL_FIELDS.forEach((k) => { next[k] = $(k).checked; });
    next.favorites = readFavRows();
    next.micDeviceId = $('micDevice').value;
    next.speakerDeviceId = $('speakerDevice').value;
    next.ringerDeviceId = $('ringerDevice').value;

    await window.centinelo.saveSettings(next);
    const st = $('saveStatus');
    st.textContent = 'Saved ✓';
    setTimeout(() => { st.textContent = ''; }, 2500);
  }

  $('saveBtn').addEventListener('click', save);

  $('micPermBtn').addEventListener('click', async () => {
    const st = $('micPermStatus');
    st.textContent = '…';
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      stream.getTracks().forEach((t) => t.stop());
      st.textContent = 'Microphone OK ✓';
      const s = await window.centinelo.getSettings();
      await fillDeviceSelects(s); // labels appear after permission
    } catch (e) {
      st.textContent = 'Microphone blocked: ' + (e && e.name);
    }
  });

  load();
})();

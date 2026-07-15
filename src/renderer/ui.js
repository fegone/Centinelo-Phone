/**
 * Centinelo Phone — main window UI.
 *
 * Ported from the original extension widget UI. The extension's remote-control
 * messaging is gone: this UI calls the in-page Engine directly and subscribes
 * to its state broadcasts. New: call-waiting bar, swap, DND toggle, dial
 * prefill from protocol/bridge/hotkey requests.
 */
(function () {
  'use strict';

  const C = self.CentineloCommon;
  const STATE = C.CALL_STATE;
  const DIR = C.DIRECTION;

  // ---- DOM refs ----
  const statusDot = document.getElementById('statusDot');
  const statusText = document.getElementById('statusText');
  const extensionLabel = document.getElementById('extensionLabel');
  const dndBtn = document.getElementById('dndBtn');
  const settingsBtn = document.getElementById('settingsBtn');
  const versionLabel = document.getElementById('versionLabel');

  const waitingBar = document.getElementById('waitingBar');
  const waitingName = document.getElementById('waitingName');
  const waitingAnswerBtn = document.getElementById('waitingAnswerBtn');
  const waitingRejectBtn = document.getElementById('waitingRejectBtn');

  const idleView = document.getElementById('idleView');
  const ringingView = document.getElementById('ringingView');
  const callingView = document.getElementById('callingView');
  const inCallView = document.getElementById('inCallView');

  const dialInput = document.getElementById('dialInput');
  const backspaceBtn = document.getElementById('backspaceBtn');
  const callBtn = document.getElementById('callBtn');
  const redialBtn = document.getElementById('redialBtn');
  const favoritesRow = document.getElementById('favoritesRow');
  const recentsList = document.getElementById('recentsList');

  const ringingName = document.getElementById('ringingName');
  const ringingNumber = document.getElementById('ringingNumber');
  const answerBtn = document.getElementById('answerBtn');
  const declineBtn = document.getElementById('declineBtn');

  const callingNumber = document.getElementById('callingNumber');
  const cancelCallBtn = document.getElementById('cancelCallBtn');

  const holdBanner = document.getElementById('holdBanner');
  const resumeBtn = document.getElementById('resumeBtn');
  const heldOtherBar = document.getElementById('heldOtherBar');
  const heldOtherName = document.getElementById('heldOtherName');
  const swapBtn = document.getElementById('swapBtn');
  const inCallName = document.getElementById('inCallName');
  const inCallNumber = document.getElementById('inCallNumber');
  const callTimer = document.getElementById('callTimer');

  const muteBtn = document.getElementById('muteBtn');
  const holdBtn = document.getElementById('holdBtn');
  const keypadBtn = document.getElementById('keypadBtn');
  const transferBtn = document.getElementById('transferBtn');
  const hangupBtn = document.getElementById('hangupBtn');

  const transferPanel = document.getElementById('transferPanel');
  const transferTarget = document.getElementById('transferTarget');
  const transferButtonsInitial = document.getElementById('transferButtonsInitial');
  const transferButtonsAttended = document.getElementById('transferButtonsAttended');
  const blindTransferBtn = document.getElementById('blindTransferBtn');
  const attendedTransferBtn = document.getElementById('attendedTransferBtn');
  const completeTransferBtn = document.getElementById('completeTransferBtn');
  const cancelTransferBtn = document.getElementById('cancelTransferBtn');
  const closeTransferBtn = document.getElementById('closeTransferBtn');

  const keypadOverlay = document.getElementById('keypadOverlay');
  const dtmfDisplay = document.getElementById('dtmfDisplay');
  const closeKeypadBtn = document.getElementById('closeKeypadBtn');

  let latestState = null;
  let timerInterval = null;
  let attendedTransferActive = false;
  let lastDialedNumber = null;

  // ---------------------------------------------------------------------
  // View switching
  // ---------------------------------------------------------------------
  function showView(name) {
    [idleView, ringingView, callingView, inCallView].forEach((v) => v.classList.add('hidden'));
    if (name === 'idle') idleView.classList.remove('hidden');
    if (name === 'ringing') ringingView.classList.remove('hidden');
    if (name === 'calling') callingView.classList.remove('hidden');
    if (name === 'in-call') inCallView.classList.remove('hidden');
  }

  // ---------------------------------------------------------------------
  // Status header
  // ---------------------------------------------------------------------
  function updateHeader(state) {
    statusDot.classList.remove('registered', 'connecting', 'disconnected');
    if (state.registered) {
      statusDot.classList.add('registered');
      statusText.textContent = state.dnd ? 'DND' : 'Registered';
    } else if (state.state === STATE.CONNECTING) {
      statusDot.classList.add('connecting');
      statusText.textContent = 'Connecting…';
    } else {
      statusDot.classList.add('disconnected');
      statusText.textContent = 'Disconnected';
    }
    extensionLabel.textContent = state.extension ? `Ext ${state.extension}` : '';
    dndBtn.classList.toggle('active', !!state.dnd);
  }

  // ---------------------------------------------------------------------
  // Timer
  // ---------------------------------------------------------------------
  function startTimer(startTs) {
    stopTimer();
    const tick = () => {
      callTimer.textContent = C.formatDuration((Date.now() - startTs) / 1000);
    };
    tick();
    timerInterval = setInterval(tick, 1000);
  }

  function stopTimer() {
    if (timerInterval) { clearInterval(timerInterval); timerInterval = null; }
  }

  // ---------------------------------------------------------------------
  // Render state
  // ---------------------------------------------------------------------
  function render(state) {
    latestState = state;
    updateHeader(state);

    if (state.prefillNumber) {
      dialInput.value = state.prefillNumber;
    }

    const name = state.callerName || null;
    const number = state.callerNumber ? C.formatUSNumber(state.callerNumber) : '';

    // Call-waiting bar (visible in any in-call state)
    const waiting = state.waitingCall;
    waitingBar.classList.toggle('hidden', !waiting);
    if (waiting) {
      waitingName.textContent = waiting.name || C.formatUSNumber(waiting.number || '') || 'Unknown';
    }

    switch (state.state) {
      case STATE.RINGING:
        showView('ringing');
        ringingName.textContent = name || 'Unknown';
        ringingNumber.textContent = number;
        stopTimer();
        break;

      case STATE.CALLING:
        showView('calling');
        callingNumber.textContent = number || state.callerNumber || '';
        stopTimer();
        break;

      case STATE.IN_CALL:
      case STATE.HELD:
        showView('in-call');
        inCallName.textContent = name || number || 'Unknown';
        inCallNumber.textContent = name ? number : '';
        if (state.callStartTs) startTimer(state.callStartTs);

        holdBanner.classList.toggle('hidden', state.state !== STATE.HELD);
        muteBtn.classList.toggle('active', !!state.muted);
        holdBtn.classList.toggle('active', state.state === STATE.HELD);

        heldOtherBar.classList.toggle('hidden', !state.heldOther);
        if (state.heldOther) {
          heldOtherName.textContent = '⏸ ' + (state.heldOther.name || C.formatUSNumber(state.heldOther.number || '') || 'Held call');
        }

        if (state.state === STATE.HELD) {
          transferPanel.classList.add('hidden');
          keypadOverlay.classList.add('hidden');
        }
        break;

      default:
        showView('idle');
        stopTimer();
        transferPanel.classList.add('hidden');
        keypadOverlay.classList.add('hidden');
        resetTransferPanel();
        break;
    }

    callBtn.disabled = !state.registered || !dialInput.value.trim();
    redialBtn.disabled = !state.registered || !lastDialedNumber;
    renderFavorites(state);
  }

  // ---------------------------------------------------------------------
  // BLF favorites
  // ---------------------------------------------------------------------
  function renderFavorites(state) {
    const favorites = state.favorites || [];
    favoritesRow.classList.toggle('hidden', !favorites.length);
    favoritesRow.innerHTML = '';
    const lampTitle = {
      [C.BLF_STATE.IDLE]: 'Available',
      [C.BLF_STATE.RINGING]: 'Ringing',
      [C.BLF_STATE.BUSY]: 'On the phone',
      [C.BLF_STATE.UNKNOWN]: 'Status unknown'
    };
    favorites.forEach((fav) => {
      const lamp = (state.blf && state.blf[fav.ext]) || C.BLF_STATE.UNKNOWN;
      const btn = document.createElement('button');
      btn.className = 'fav-btn blf-' + lamp;
      btn.title = `${fav.label || 'Ext ' + fav.ext} — ${lampTitle[lamp]}. Click to call.`;
      const dot = document.createElement('span');
      dot.className = 'fav-lamp';
      const label = document.createElement('span');
      label.className = 'fav-label';
      label.textContent = fav.label || fav.ext;
      btn.appendChild(dot);
      btn.appendChild(label);
      btn.addEventListener('click', () => {
        if (!latestState || !latestState.registered) return;
        Engine.dial(fav.ext);
      });
      favoritesRow.appendChild(btn);
    });
  }

  // ---------------------------------------------------------------------
  // Idle view: dialpad + call
  // ---------------------------------------------------------------------
  function refreshCallBtn() {
    callBtn.disabled = !latestState || !latestState.registered || !dialInput.value.trim();
  }

  document.querySelectorAll('#idleView .dial-key').forEach((btn) => {
    btn.addEventListener('click', () => {
      dialInput.value += btn.dataset.key;
      dialInput.focus();
      refreshCallBtn();
    });
  });

  dialInput.addEventListener('input', refreshCallBtn);
  dialInput.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !callBtn.disabled) doDial();
  });

  backspaceBtn.addEventListener('click', () => {
    dialInput.value = dialInput.value.slice(0, -1);
    refreshCallBtn();
  });

  callBtn.addEventListener('click', doDial);

  function doDial() {
    const number = dialInput.value.trim();
    if (!number) return;
    Engine.dial(number);
    dialInput.value = '';
  }

  redialBtn.addEventListener('click', () => {
    if (!lastDialedNumber || !latestState || !latestState.registered) return;
    Engine.dial(lastDialedNumber);
  });

  // ---------------------------------------------------------------------
  // Ringing / calling views
  // ---------------------------------------------------------------------
  answerBtn.addEventListener('click', () => Engine.answer());
  declineBtn.addEventListener('click', () => Engine.reject());
  cancelCallBtn.addEventListener('click', () => Engine.hangup());

  // ---------------------------------------------------------------------
  // Call waiting
  // ---------------------------------------------------------------------
  waitingAnswerBtn.addEventListener('click', () => Engine.answerWaiting());
  waitingRejectBtn.addEventListener('click', () => Engine.rejectWaiting());
  swapBtn.addEventListener('click', () => Engine.swapCalls());

  // ---------------------------------------------------------------------
  // In-call controls
  // ---------------------------------------------------------------------
  muteBtn.addEventListener('click', () => {
    (latestState && latestState.muted) ? Engine.unmute() : Engine.mute();
  });

  holdBtn.addEventListener('click', () => {
    (latestState && latestState.state === STATE.HELD) ? Engine.unhold() : Engine.hold();
  });

  resumeBtn.addEventListener('click', () => Engine.unhold());
  hangupBtn.addEventListener('click', () => Engine.hangup());

  keypadBtn.addEventListener('click', () => {
    transferPanel.classList.add('hidden');
    keypadOverlay.classList.toggle('hidden');
  });

  closeKeypadBtn.addEventListener('click', () => {
    keypadOverlay.classList.add('hidden');
    dtmfDisplay.textContent = '';
  });

  document.querySelectorAll('.dtmf-key').forEach((btn) => {
    btn.addEventListener('click', () => {
      const tone = btn.dataset.key;
      Engine.dtmf(tone);
      dtmfDisplay.textContent += tone;
    });
  });

  // ---------------------------------------------------------------------
  // Transfer panel
  // ---------------------------------------------------------------------
  function resetTransferPanel() {
    attendedTransferActive = false;
    transferTarget.value = '';
    transferButtonsInitial.classList.remove('hidden');
    transferButtonsAttended.classList.add('hidden');
  }

  transferBtn.addEventListener('click', () => {
    keypadOverlay.classList.add('hidden');
    transferPanel.classList.toggle('hidden');
    if (!transferPanel.classList.contains('hidden')) {
      resetTransferPanel();
      transferTarget.focus();
    }
  });

  closeTransferBtn.addEventListener('click', () => {
    if (attendedTransferActive) Engine.attendedTransferCancel();
    transferPanel.classList.add('hidden');
    resetTransferPanel();
  });

  blindTransferBtn.addEventListener('click', () => {
    const target = transferTarget.value.trim();
    if (!target) return;
    Engine.blindTransfer(target);
    transferPanel.classList.add('hidden');
    resetTransferPanel();
  });

  attendedTransferBtn.addEventListener('click', () => {
    const target = transferTarget.value.trim();
    if (!target) return;
    attendedTransferActive = true;
    transferButtonsInitial.classList.add('hidden');
    transferButtonsAttended.classList.remove('hidden');
    Engine.attendedTransferStart(target);
  });

  completeTransferBtn.addEventListener('click', () => {
    Engine.attendedTransferComplete();
    transferPanel.classList.add('hidden');
    resetTransferPanel();
  });

  cancelTransferBtn.addEventListener('click', () => {
    Engine.attendedTransferCancel();
    transferPanel.classList.add('hidden');
    resetTransferPanel();
  });

  // ---------------------------------------------------------------------
  // Header buttons
  // ---------------------------------------------------------------------
  settingsBtn.addEventListener('click', () => window.centinelo.openSettings());

  dndBtn.addEventListener('click', async () => {
    const s = await window.centinelo.getSettings();
    await window.centinelo.saveSettings({ dnd: !s.dnd });
  });

  // ---------------------------------------------------------------------
  // Recent calls
  // ---------------------------------------------------------------------
  async function loadRecents() {
    renderRecents(await window.centinelo.getHistory());
  }

  function renderRecents(history) {
    const lastOut = (history || []).find((e) => e.direction === DIR.OUTBOUND && e.number);
    lastDialedNumber = lastOut ? lastOut.number : null;
    redialBtn.disabled = !latestState || !latestState.registered || !lastDialedNumber;
    redialBtn.title = lastDialedNumber
      ? `Redial ${C.formatUSNumber(lastDialedNumber)}`
      : 'Redial last number';
    recentsList.innerHTML = '';
    if (!history || !history.length) {
      const li = document.createElement('li');
      li.className = 'recent-empty';
      li.textContent = 'No recent calls';
      recentsList.appendChild(li);
      return;
    }

    history.forEach((entry) => {
      const li = document.createElement('li');
      li.className = 'recent-item';

      const dirIcon = document.createElement('span');
      let dirClass = entry.direction === DIR.INBOUND ? 'inbound' : 'outbound';
      let dirSymbol = entry.direction === DIR.INBOUND ? '↘' : '↗';
      if (entry.status === 'missed') {
        dirClass = 'missed';
        dirSymbol = '✕';
      }
      dirIcon.className = 'recent-dir ' + dirClass;
      dirIcon.textContent = dirSymbol;

      const info = document.createElement('div');
      info.className = 'recent-info';

      const nameEl = document.createElement('div');
      nameEl.className = 'recent-name';
      nameEl.textContent = entry.name || C.formatUSNumber(entry.number) || 'Unknown';

      const metaEl = document.createElement('div');
      metaEl.className = 'recent-meta';
      const timeStr = new Date(entry.time).toLocaleString([], {
        month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'
      });
      const durStr =
        entry.status === 'missed' ? 'Missed'
          : entry.duration != null ? C.formatDuration(entry.duration) : '';
      metaEl.textContent = [timeStr, durStr].filter(Boolean).join(' · ');

      info.appendChild(nameEl);
      info.appendChild(metaEl);
      li.appendChild(dirIcon);
      li.appendChild(info);

      li.addEventListener('click', () => {
        dialInput.value = entry.number;
        refreshCallBtn();
        dialInput.focus();
      });

      recentsList.appendChild(li);
    });
  }

  window.centinelo.onHistoryChanged((history) => renderRecents(history));

  // ---------------------------------------------------------------------
  // Boot
  // ---------------------------------------------------------------------
  Engine.onState(render);
  loadRecents();
  window.centinelo.appVersion().then((v) => { versionLabel.textContent = 'Centinelo Phone v' + v; });
})();

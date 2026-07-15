/**
 * Centinelo Phone — SIP engine (renderer).
 *
 * Ported from a battle-tested Chrome-extension SIP engine. In Electron the engine and the
 * UI share one renderer, so the extension's messaging layer is gone: the UI
 * calls Engine.* directly and subscribes with Engine.onState(). The main
 * window is never destroyed (close = hide) so calls survive any UI action.
 *
 * New vs v2.1.0: call waiting (2nd incoming line + swap), DND, auto-answer,
 * echoCancellation/noiseSuppression/autoGainControl constraints, missed-call
 * notifications, settings hot-reload.
 *
 * NO PHI IN LOGS unless settings.debug is explicitly enabled.
 */
(function () {
  'use strict';

  const C = self.CentineloCommon;
  const STATE = C.CALL_STATE;
  const DIR = C.DIRECTION;

  let settings = {};
  let debug = false;

  let userAgent = null;
  let registerer = null;
  let currentSession = null;         // active/held call (SIP.Inviter | SIP.Invitation)
  let heldSession = null;            // second line kept on hold (call waiting answered)
  let waitingInvitation = null;      // second incoming, not yet answered
  let attendedTransferSession = null;

  let callState = STATE.DISCONNECTED;
  let callDirection = null;
  let callerName = null;
  let callerNumber = null;
  let callStartTs = null;
  let isMuted = false;
  let isHeld = false;
  let heldOtherInfo = null;          // { name, number } of the held second line
  let waitingInfo = null;            // { name, number } of the unanswered waiting call
  let autoAnswerTimer = null;

  // WSS reconnection backoff
  let reconnectAttempt = 0;
  let reconnectTimer = null;
  const RECONNECT_MIN_MS = 1000;
  const RECONNECT_MAX_MS = 30000;
  let intentionalStop = false;

  const remoteAudioEl = document.getElementById('remoteAudio');
  const ringtoneAudioEl = document.getElementById('ringtoneAudio');

  // ---- WebAudio-generated ringtone / call-waiting beep ----
  let ringAudioCtx = null;
  let ringGainNode = null;
  let ringPatternTimer = null;
  let waitingBeepTimer = null;
  let ringtoneMediaStreamDest = null;

  const stateListeners = new Set();

  function log(event, extra) { C.logEvent(debug, event, extra); }
  function logErr(event, err) { C.logError(debug, event, err); }

  function snapshotState() {
    return {
      state: callState,
      direction: callDirection,
      callerName,
      callerNumber,
      callStartTs,
      muted: isMuted,
      held: isHeld,
      registered: !!registerer && registerer.state === SIP.RegistererState.Registered,
      extension: settings.extension || null,
      wssConnected: !!(userAgent && userAgent.isConnected && userAgent.isConnected()),
      favorites: (settings.favorites || []).slice(0, C.MAX_FAVORITES),
      blf: { ...blfStatus },
      dnd: !!settings.dnd,
      waitingCall: waitingInfo ? { ...waitingInfo } : null,
      heldOther: heldOtherInfo ? { ...heldOtherInfo } : null
    };
  }

  function broadcastState() {
    const payload = snapshotState();
    stateListeners.forEach((fn) => {
      try { fn(payload); } catch (e) { /* listener errors never break the engine */ }
    });
    try { window.centinelo.reportCallState(payload); } catch (e) { /* noop */ }
  }

  function setState(next) {
    callState = next;
    log('state-change:' + next);
    broadcastState();
  }

  // ---------------------------------------------------------------------
  // Settings
  // ---------------------------------------------------------------------
  async function loadSettings() {
    settings = await window.centinelo.getSettings();
    debug = !!settings.debug;
  }

  window.centinelo.onSettingsChanged(async (next) => {
    const prev = settings;
    settings = next;
    debug = !!settings.debug;
    const connChanged = prev.wssUrl !== next.wssUrl ||
      prev.extension !== next.extension ||
      prev.password !== next.password ||
      JSON.stringify(prev.iceServers || []) !== JSON.stringify(next.iceServers || []);
    const favsChanged = JSON.stringify(prev.favorites || []) !== JSON.stringify(next.favorites || []);
    const devsChanged = prev.micDeviceId !== next.micDeviceId ||
      prev.speakerDeviceId !== next.speakerDeviceId ||
      prev.ringerDeviceId !== next.ringerDeviceId;

    if (connChanged) {
      reconnectAttempt = 0;
      await startEngine();
    } else if (favsChanged && userAgent) {
      startBlfSubscriptions(userAgent);
    }
    if (devsChanged) {
      await applyDeviceSettings({
        micDeviceId: next.micDeviceId,
        speakerDeviceId: next.speakerDeviceId,
        ringerDeviceId: next.ringerDeviceId
      });
    }
    broadcastState();
  });

  // ---------------------------------------------------------------------
  // Ringtone (2s on / 4s off) + call-waiting beep (short blip every 5s)
  // ---------------------------------------------------------------------
  function ensureRingAudioGraph() {
    if (ringAudioCtx) return;
    ringAudioCtx = new AudioContext();
    ringGainNode = ringAudioCtx.createGain();
    ringGainNode.gain.value = 0;
    ringtoneMediaStreamDest = ringAudioCtx.createMediaStreamDestination();
    ringGainNode.connect(ringtoneMediaStreamDest);
    ringtoneAudioEl.srcObject = ringtoneMediaStreamDest.stream;
  }

  function playRingChord(durationSec, gain) {
    const now = ringAudioCtx.currentTime;
    const freqs = [440, 480];
    const oscs = freqs.map((f) => {
      const osc = ringAudioCtx.createOscillator();
      osc.type = 'sine';
      osc.frequency.value = f;
      osc.connect(ringGainNode);
      return osc;
    });
    const g = gain || 0.18;
    ringGainNode.gain.cancelScheduledValues(now);
    ringGainNode.gain.setValueAtTime(0, now);
    ringGainNode.gain.linearRampToValueAtTime(g, now + 0.05);
    ringGainNode.gain.setValueAtTime(g, now + durationSec - 0.05);
    ringGainNode.gain.linearRampToValueAtTime(0, now + durationSec);
    oscs.forEach((o) => { o.start(now); o.stop(now + durationSec); });
  }

  async function startRingtone() {
    try {
      ensureRingAudioGraph();
      if (ringAudioCtx.state === 'suspended') await ringAudioCtx.resume();
      await applySinkId(ringtoneAudioEl, settings.ringerDeviceId);
      ringtoneAudioEl.play().catch((e) => logErr('ringtone-play-failed', e));
      const cycle = () => {
        playRingChord(2);
        ringPatternTimer = setTimeout(cycle, 6000);
      };
      cycle();
    } catch (e) {
      logErr('ringtone-start-failed', e);
    }
  }

  function stopRingtone() {
    if (ringPatternTimer) { clearTimeout(ringPatternTimer); ringPatternTimer = null; }
    if (ringGainNode && ringAudioCtx) {
      const now = ringAudioCtx.currentTime;
      ringGainNode.gain.cancelScheduledValues(now);
      ringGainNode.gain.setValueAtTime(0, now);
    }
    try { ringtoneAudioEl.pause(); } catch (e) { /* noop */ }
  }

  async function startWaitingBeep() {
    try {
      ensureRingAudioGraph();
      if (ringAudioCtx.state === 'suspended') await ringAudioCtx.resume();
      await applySinkId(ringtoneAudioEl, settings.ringerDeviceId);
      ringtoneAudioEl.play().catch(() => {});
      const cycle = () => {
        playRingChord(0.25, 0.10); // short, quiet blip — the user is on a call
        waitingBeepTimer = setTimeout(cycle, 5000);
      };
      cycle();
    } catch (e) {
      logErr('waiting-beep-failed', e);
    }
  }

  function stopWaitingBeep() {
    if (waitingBeepTimer) { clearTimeout(waitingBeepTimer); waitingBeepTimer = null; }
  }

  // ---------------------------------------------------------------------
  // Audio device routing
  // ---------------------------------------------------------------------
  async function applySinkId(audioEl, deviceId) {
    if (!deviceId || deviceId === 'default') return;
    if (typeof audioEl.setSinkId !== 'function') return;
    try { await audioEl.setSinkId(deviceId); } catch (e) { logErr('set-sink-id-failed', e); }
  }

  function attachRemoteStreamToAudioEl(session) {
    const sdh = session.sessionDescriptionHandler;
    if (!sdh || !sdh.peerConnection) return;
    const pc = sdh.peerConnection;
    const remoteStream = new MediaStream();
    pc.getReceivers().forEach((r) => { if (r.track) remoteStream.addTrack(r.track); });
    remoteAudioEl.srcObject = remoteStream;
    applySinkId(remoteAudioEl, settings.speakerDeviceId);
    remoteAudioEl.play().catch((e) => logErr('remote-audio-play-failed', e));
  }

  function getLocalAudioTrack(session) {
    const sdh = session.sessionDescriptionHandler;
    if (!sdh || !sdh.peerConnection) return null;
    const sender = sdh.peerConnection.getSenders().find((s) => s.track && s.track.kind === 'audio');
    return sender ? sender.track : null;
  }

  // ---------------------------------------------------------------------
  // Media constraints (mic device + audio processing toggles)
  // ---------------------------------------------------------------------
  function mediaConstraints() {
    const audio = {
      echoCancellation: settings.echoCancellation !== false,
      noiseSuppression: settings.noiseSuppression !== false,
      autoGainControl: settings.autoGainControl !== false
    };
    if (settings.micDeviceId && settings.micDeviceId !== 'default') {
      audio.deviceId = { exact: settings.micDeviceId };
    }
    return { audio, video: false };
  }

  // ---------------------------------------------------------------------
  // UserAgent / Registerer lifecycle
  // ---------------------------------------------------------------------
  function buildUserAgentOptions() {
    const uri = SIP.UserAgent.makeURI(`sip:${settings.extension}@${extractHost(settings.wssUrl)}`);
    if (!uri) throw new Error('Invalid extension/server configuration');
    return {
      uri,
      transportOptions: {
        server: settings.wssUrl,
        connectionTimeout: 10,
        traceSip: false
      },
      contactParams: { transport: 'wss' },
      // Build signature in the SIP User-Agent header: lets us verify FROM THE
      // PBX (pjsip logger) exactly which client version a remote PC runs.
      userAgentString: 'CentineloPhone/' + C.APP_VERSION,
      authorizationUsername: settings.extension,
      authorizationPassword: settings.password,
      displayName: settings.displayName || settings.extension,
      logLevel: debug ? 'debug' : 'error',
      logBuiltinEnabled: debug,
      sessionDescriptionHandlerFactoryOptions: {
        iceGatheringTimeout: 500, // LAN/Tailscale — no external ICE needed
        peerConnectionConfiguration: {
          iceServers: (settings.iceServers && settings.iceServers.length ? settings.iceServers : [])
        }
      },
      delegate: {
        // onDisconnect intentionally NOT here — set in startEngine with an
        // identity guard (userAgent === ua). A global delegate fires reconnect
        // when teardownUserAgent() stops the OLD UA during a reconnect →
        // 1-second register/unregister loop (seen live 2026-07-10).
        onInvite: handleIncomingInvite
      }
    };
  }

  function extractHost(wssUrl) {
    try { return new URL(wssUrl).hostname; } catch (e) { return wssUrl; }
  }

  async function startEngine() {
    intentionalStop = false;
    await loadSettings();

    if (!settings.wssUrl || !settings.extension || !settings.password) {
      log('start-skipped-incomplete-settings');
      setState(STATE.DISCONNECTED);
      return;
    }

    await teardownUserAgent();
    setState(STATE.CONNECTING);

    try {
      const options = buildUserAgentOptions();
      const ua = new SIP.UserAgent(options);
      userAgent = ua;

      ua.transport.onConnect = () => {
        if (userAgent !== ua) return; // event from a replaced UA
        log('transport-connected');
        reconnectAttempt = 0;
      };
      ua.transport.onDisconnect = (err) => {
        if (userAgent !== ua) return; // old UA teardown: do NOT reschedule
        logErr('transport-disconnected', err);
        if (!intentionalStop) {
          setState(STATE.DISCONNECTED);
          scheduleReconnect();
        }
      };

      await userAgent.start();
      log('useragent-started');

      registerer = new SIP.Registerer(userAgent, { expires: 300 });
      registerer.stateChange.addListener((state) => {
        log('registerer-state:' + state);
        if (state === SIP.RegistererState.Registered) {
          if (callState === STATE.CONNECTING || callState === STATE.DISCONNECTED) {
            setState(STATE.REGISTERED);
          } else {
            broadcastState();
          }
          startBlfSubscriptions(ua);
        } else if (state === SIP.RegistererState.Unregistered) {
          if (!isInActiveCall()) setState(STATE.DISCONNECTED);
        }
      });

      await registerer.register();
    } catch (e) {
      logErr('start-engine-failed', e);
      setState(STATE.DISCONNECTED);
      if (!intentionalStop) scheduleReconnect();
    }
  }

  // ---------------------------------------------------------------------
  // BLF (Busy Lamp Field) — SUBSCRIBE Event:dialog per favorite hint.
  // ---------------------------------------------------------------------
  let blfSubscribers = [];
  let blfStatus = {};

  function blfDialogStateToLamp(xmlText) {
    try {
      const doc = new DOMParser().parseFromString(xmlText, 'application/xml');
      const states = [...doc.getElementsByTagName('state')].map((n) => (n.textContent || '').trim().toLowerCase());
      if (!states.length) return C.BLF_STATE.IDLE;
      if (states.some((s) => s === 'confirmed')) return C.BLF_STATE.BUSY;
      if (states.some((s) => s === 'early' || s === 'proceeding' || s === 'trying')) return C.BLF_STATE.RINGING;
      return C.BLF_STATE.IDLE;
    } catch (e) {
      logErr('blf-parse-failed', e);
      return C.BLF_STATE.UNKNOWN;
    }
  }

  function stopBlfSubscriptions() {
    for (const { subscriber } of blfSubscribers) {
      try { subscriber.dispose(); } catch (e) { /* ignore */ }
    }
    blfSubscribers = [];
    blfStatus = {};
  }

  function startBlfSubscriptions(ua) {
    if (userAgent !== ua) return;
    stopBlfSubscriptions();
    const favorites = (settings.favorites || []).filter((f) => f && f.ext).slice(0, C.MAX_FAVORITES);
    if (!favorites.length) { broadcastState(); return; }
    const host = extractHost(settings.wssUrl);
    for (const fav of favorites) {
      const target = SIP.UserAgent.makeURI(`sip:${fav.ext}@${host}`);
      if (!target) continue;
      blfStatus[fav.ext] = C.BLF_STATE.UNKNOWN;
      try {
        const subscriber = new SIP.Subscriber(ua, target, 'dialog', { expires: 3600 });
        subscriber.delegate = {
          onNotify(notification) {
            try {
              const body = notification.request.body || '';
              const lamp = body ? blfDialogStateToLamp(body) : blfStatus[fav.ext];
              if (blfStatus[fav.ext] !== lamp) {
                blfStatus[fav.ext] = lamp;
                log('blf-lamp', { ext: fav.ext, lamp });
                broadcastState();
              }
              notification.accept();
            } catch (e) {
              logErr('blf-notify-failed', e);
              try { notification.accept(); } catch (e2) { /* ignore */ }
            }
          }
        };
        subscriber.subscribe().catch((e) => {
          logErr('blf-subscribe-failed:' + fav.ext, e);
          blfStatus[fav.ext] = C.BLF_STATE.UNKNOWN;
          broadcastState();
        });
        blfSubscribers.push({ ext: fav.ext, subscriber });
      } catch (e) {
        logErr('blf-setup-failed:' + fav.ext, e);
      }
    }
    broadcastState();
  }

  async function teardownUserAgent() {
    stopBlfSubscriptions();
    if (registerer) {
      try { await registerer.unregister(); } catch (e) { /* ignore */ }
      registerer = null;
    }
    if (userAgent) {
      try {
        // Silence the outgoing UA's handlers BEFORE stop(): stop() disconnects
        // the transport and without this the old onDisconnect schedules a reconnect.
        userAgent.transport.onConnect = undefined;
        userAgent.transport.onDisconnect = undefined;
      } catch (e) { /* ignore */ }
      try { await userAgent.stop(); } catch (e) { /* ignore */ }
      userAgent = null;
    }
  }

  // Circuit breaker: if anything (a bug, the network, the PBX) causes burst
  // reconnects, NEVER hammer the registrar. >6 reconnects in 60s => clamp 30s.
  const RECONNECT_BURST_WINDOW_MS = 60000;
  const RECONNECT_BURST_MAX = 6;
  let reconnectTimestamps = [];

  function scheduleReconnect() {
    if (reconnectTimer) return;
    const now = Date.now();
    reconnectTimestamps = reconnectTimestamps.filter((t) => now - t < RECONNECT_BURST_WINDOW_MS);
    reconnectTimestamps.push(now);
    let delay = Math.min(RECONNECT_MIN_MS * Math.pow(2, reconnectAttempt), RECONNECT_MAX_MS);
    if (reconnectTimestamps.length > RECONNECT_BURST_MAX) {
      delay = RECONNECT_MAX_MS;
      logErr('reconnect-burst-detected', { count: reconnectTimestamps.length, clampedDelayMs: delay });
    }
    reconnectAttempt += 1;
    log('reconnect-scheduled', { delayMs: delay });
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      startEngine();
    }, delay);
  }

  window.addEventListener('online', () => {
    log('network-online');
    if (!isInActiveCall() && (!userAgent || !userAgent.isConnected || !userAgent.isConnected())) {
      reconnectAttempt = 0;
      if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null; }
      startEngine();
    }
  });
  window.addEventListener('offline', () => log('network-offline'));

  function isInActiveCall() {
    return callState === STATE.RINGING || callState === STATE.CALLING ||
      callState === STATE.IN_CALL || callState === STATE.HELD;
  }

  // ---------------------------------------------------------------------
  // Inbound calls (with DND, auto-answer, and call waiting)
  // ---------------------------------------------------------------------
  function inviteCallerInfo(invitation) {
    const remoteIdentity = invitation.remoteIdentity;
    return {
      number: (remoteIdentity && remoteIdentity.uri && remoteIdentity.uri.user) || 'Unknown',
      name: (remoteIdentity && remoteIdentity.displayName) || null
    };
  }

  async function handleIncomingInvite(invitation) {
    const info = inviteCallerInfo(invitation);

    // DND: silently decline, keep a missed entry so nothing is ever lost.
    if (settings.dnd) {
      try { await invitation.reject({ statusCode: 486 }); } catch (e) { /* noop */ }
      addHistoryEntry({
        name: info.name, number: info.number, direction: DIR.INBOUND,
        time: Date.now(), duration: 0, status: 'missed'
      });
      log('invite-rejected-dnd');
      return;
    }

    // Call waiting: a second incoming while on a call becomes the waiting line.
    if (isInActiveCall()) {
      if (settings.callWaiting !== false && !waitingInvitation &&
          (callState === STATE.IN_CALL || callState === STATE.HELD)) {
        waitingInvitation = invitation;
        waitingInfo = { name: info.name, number: info.number };
        startWaitingBeep();
        broadcastState();
        lookupAndTag(invitation, info.number, (name) => {
          if (waitingInvitation === invitation) {
            waitingInfo = { name, number: info.number };
            broadcastState();
          }
        });
        invitation.stateChange.addListener((state) => {
          if (state === SIP.SessionState.Terminated && waitingInvitation === invitation) {
            // Caller gave up while waiting.
            addHistoryEntry({
              name: waitingInfo && waitingInfo.name, number: info.number,
              direction: DIR.INBOUND, time: Date.now(), duration: 0, status: 'missed'
            });
            clearWaiting();
          }
        });
        window.centinelo.notify({
          kind: 'incoming',
          title: 'Call waiting',
          body: (waitingInfo.name || C.formatUSNumber(info.number))
        });
        return;
      }
      // Single-line behavior (call waiting off / already have a waiting call).
      try { await invitation.reject({ statusCode: 486 }); } catch (e) { logErr('reject-busy-failed', e); }
      return;
    }

    currentSession = invitation;
    callDirection = DIR.INBOUND;
    callerNumber = info.number;
    callerName = info.name;

    setState(STATE.RINGING);
    startRingtone();

    lookupAndTag(invitation, callerNumber, (name) => {
      if (currentSession === invitation && callState === STATE.RINGING) {
        callerName = name;
        broadcastState();
      }
    });

    invitation.stateChange.addListener((state) => {
      if (state === SIP.SessionState.Terminated && currentSession === invitation) {
        onSessionEnded();
      }
    });

    window.centinelo.notify({
      kind: 'incoming',
      title: 'Incoming call',
      body: (callerName || C.formatUSNumber(callerNumber))
    });

    // Auto-answer (opt-in, headset workflows).
    if (settings.autoAnswer) {
      const delay = Math.max(0, Number(settings.autoAnswerDelayMs) || 0);
      autoAnswerTimer = setTimeout(() => {
        autoAnswerTimer = null;
        if (currentSession === invitation && callState === STATE.RINGING) {
          log('auto-answering');
          answerCall();
        }
      }, delay);
    }
  }

  function lookupAndTag(session, number, apply) {
    window.centinelo.lookup(number)
      .then((resp) => { if (resp && resp.name) apply(resp.name); })
      .catch(() => { /* lookup unavailable — number-only display */ });
  }

  function clearAutoAnswerTimer() {
    if (autoAnswerTimer) { clearTimeout(autoAnswerTimer); autoAnswerTimer = null; }
  }

  function clearWaiting() {
    stopWaitingBeep();
    waitingInvitation = null;
    waitingInfo = null;
    broadcastState();
  }

  async function answerCall() {
    if (!currentSession || callState !== STATE.RINGING) return;
    clearAutoAnswerTimer();
    stopRingtone();
    try {
      await currentSession.accept({
        sessionDescriptionHandlerOptions: { constraints: mediaConstraints() }
      });
      onCallEstablished();
    } catch (e) {
      logErr('answer-failed', e);
      onSessionEnded();
    }
  }

  async function rejectCall() {
    if (!currentSession) return;
    clearAutoAnswerTimer();
    stopRingtone();
    try {
      if (callState === STATE.RINGING) {
        if (typeof currentSession.reject === 'function') {
          await currentSession.reject();
        } else if (typeof currentSession.cancel === 'function') {
          await currentSession.cancel();
        }
      }
    } catch (e) {
      logErr('reject-failed', e);
    }
    onSessionEnded();
  }

  // ---- Call waiting actions ----
  async function answerWaiting() {
    if (!waitingInvitation) return;
    const invitation = waitingInvitation;
    const info = waitingInfo || inviteCallerInfo(invitation);
    stopWaitingBeep();
    try {
      // Park the current call on hold, then answer the waiting line.
      if (currentSession && currentSession.state === SIP.SessionState.Established && !isHeld) {
        await holdSession(currentSession, true);
      }
      heldSession = currentSession;
      heldOtherInfo = { name: callerName, number: callerNumber };

      waitingInvitation = null;
      waitingInfo = null;
      currentSession = invitation;
      callDirection = DIR.INBOUND;
      callerName = info.name;
      callerNumber = info.number;

      invitation.stateChange.addListener((state) => {
        if (state === SIP.SessionState.Terminated && currentSession === invitation) {
          onSessionEnded();
        }
      });

      await invitation.accept({
        sessionDescriptionHandlerOptions: { constraints: mediaConstraints() }
      });
      onCallEstablished();
    } catch (e) {
      logErr('answer-waiting-failed', e);
      clearWaiting();
    }
  }

  async function rejectWaiting() {
    if (!waitingInvitation) return;
    const invitation = waitingInvitation;
    const info = waitingInfo || {};
    try { await invitation.reject({ statusCode: 486 }); } catch (e) { /* noop */ }
    addHistoryEntry({
      name: info.name, number: info.number, direction: DIR.INBOUND,
      time: Date.now(), duration: 0, status: 'missed'
    });
    clearWaiting();
  }

  async function swapCalls() {
    if (!heldSession || heldSession.state !== SIP.SessionState.Established) return;
    if (!currentSession || currentSession.state !== SIP.SessionState.Established) return;
    try {
      await holdSession(currentSession, true);
      const prevCurrent = currentSession;
      const prevInfo = { name: callerName, number: callerNumber };

      currentSession = heldSession;
      heldSession = prevCurrent;
      callerName = heldOtherInfo && heldOtherInfo.name;
      callerNumber = heldOtherInfo && heldOtherInfo.number;
      heldOtherInfo = prevInfo;

      await holdSession(currentSession, false);
      isHeld = false;
      attachRemoteStreamToAudioEl(currentSession);
      setState(STATE.IN_CALL);
    } catch (e) {
      logErr('swap-calls-failed', e);
    }
  }

  // Low-level hold on an arbitrary session (used by swap/waiting).
  async function holdSession(session, hold) {
    const sessionDescriptionHandlerModifiers = hold ? [SIP.Web.holdModifier] : [];
    await session.invite({ sessionDescriptionHandlerModifiers });
  }

  // ---------------------------------------------------------------------
  // Outbound calls
  // ---------------------------------------------------------------------
  async function dial(number) {
    if (isInActiveCall()) { log('dial-ignored-active-call'); return; }
    if (!userAgent || !registerer || registerer.state !== SIP.RegistererState.Registered) {
      log('dial-ignored-not-registered');
      return;
    }
    if (!number) return;

    const host = extractHost(settings.wssUrl);
    const target = SIP.UserAgent.makeURI(`sip:${number}@${host}`);
    if (!target) { logErr('dial-invalid-target'); return; }

    callDirection = DIR.OUTBOUND;
    callerNumber = number;
    callerName = null;

    try {
      const inviter = new SIP.Inviter(userAgent, target, {
        sessionDescriptionHandlerOptions: { constraints: mediaConstraints() }
      });
      currentSession = inviter;

      inviter.stateChange.addListener((state) => {
        if (state === SIP.SessionState.Established && currentSession === inviter) {
          onCallEstablished();
        } else if (state === SIP.SessionState.Terminated && currentSession === inviter) {
          onSessionEnded();
        }
      });

      setState(STATE.CALLING);
      await inviter.invite();
    } catch (e) {
      logErr('dial-failed', e);
      onSessionEnded();
    }
  }

  // ---------------------------------------------------------------------
  // Established call / hangup
  // ---------------------------------------------------------------------
  function onCallEstablished() {
    stopRingtone();
    callStartTs = Date.now();
    isMuted = false;
    isHeld = false;
    setState(STATE.IN_CALL);
    if (currentSession) attachRemoteStreamToAudioEl(currentSession);
    addHistoryEntry({
      name: callerName,
      number: callerNumber,
      direction: callDirection,
      time: callStartTs,
      duration: null,
      status: 'connected'
    });
  }

  async function hangup() {
    stopRingtone();
    const session = currentSession;
    if (!session) return;
    try {
      const state = session.state;
      if (state === SIP.SessionState.Established) {
        await session.bye();
      } else if (state === SIP.SessionState.Establishing) {
        if (callDirection === DIR.OUTBOUND && typeof session.cancel === 'function') {
          await session.cancel();
        } else if (typeof session.reject === 'function') {
          await session.reject();
        }
      }
    } catch (e) {
      logErr('hangup-failed', e);
    }
    onSessionEnded();
  }

  function onSessionEnded() {
    clearAutoAnswerTimer();
    stopRingtone();
    if (callStartTs && (callState === STATE.IN_CALL || callState === STATE.HELD)) {
      updateLastHistoryDuration(Date.now() - callStartTs);
    } else if (callDirection === DIR.INBOUND && callState === STATE.RINGING) {
      addHistoryEntry({
        name: callerName,
        number: callerNumber,
        direction: callDirection,
        time: Date.now(),
        duration: 0,
        status: 'missed'
      });
      window.centinelo.notify({
        kind: 'missed',
        title: 'Missed call',
        body: (callerName || C.formatUSNumber(callerNumber || ''))
      });
    }

    currentSession = null;
    attendedTransferSession = null;
    callDirection = null;
    callerName = null;
    callerNumber = null;
    callStartTs = null;
    isMuted = false;
    isHeld = false;
    remoteAudioEl.srcObject = null;

    // A held second line survives the hangup: promote it (still on hold —
    // the user resumes when ready).
    if (heldSession && heldSession.state === SIP.SessionState.Established) {
      currentSession = heldSession;
      heldSession = null;
      callerName = heldOtherInfo && heldOtherInfo.name;
      callerNumber = heldOtherInfo && heldOtherInfo.number;
      heldOtherInfo = null;
      callDirection = DIR.INBOUND;
      callStartTs = Date.now(); // resumed-leg timer restarts
      isHeld = true;
      currentSession.stateChange.addListener((state) => {
        if (state === SIP.SessionState.Terminated && currentSession) {
          onSessionEnded();
        }
      });
      setState(STATE.HELD);
      return;
    }
    heldSession = null;
    heldOtherInfo = null;

    const nextState =
      registerer && registerer.state === SIP.RegistererState.Registered
        ? STATE.REGISTERED
        : STATE.DISCONNECTED;
    setState(nextState);
  }

  // ---------------------------------------------------------------------
  // Mute / Hold / DTMF
  // ---------------------------------------------------------------------
  function setMute(muted) {
    if (!currentSession) return;
    const track = getLocalAudioTrack(currentSession);
    if (track) track.enabled = !muted;
    isMuted = muted;
    broadcastState();
  }

  async function setHold(hold) {
    if (!currentSession || currentSession.state !== SIP.SessionState.Established) return;
    try {
      const sessionDescriptionHandlerModifiers = hold ? [SIP.Web.holdModifier] : [];
      await currentSession.invite({
        sessionDescriptionHandlerModifiers,
        requestDelegate: {
          onAccept: () => {
            isHeld = hold;
            if (hold) {
              stopRingtone();
            } else if (currentSession) {
              attachRemoteStreamToAudioEl(currentSession);
            }
            setState(hold ? STATE.HELD : STATE.IN_CALL);
          }
        }
      });
    } catch (e) {
      logErr('hold-toggle-failed', e);
    }
  }

  function sendDtmf(tone) {
    if (!currentSession || currentSession.state !== SIP.SessionState.Established) return;
    try {
      const sdh = currentSession.sessionDescriptionHandler;
      if (sdh && typeof sdh.sendDtmf === 'function') {
        sdh.sendDtmf(tone, { duration: 100, interToneGap: 70 });
      }
    } catch (e) {
      logErr('dtmf-failed', e);
    }
  }

  // ---------------------------------------------------------------------
  // Transfers
  // ---------------------------------------------------------------------
  async function blindTransfer(target) {
    if (!currentSession || currentSession.state !== SIP.SessionState.Established) return;
    try {
      const host = extractHost(settings.wssUrl);
      const targetUri = SIP.UserAgent.makeURI(`sip:${target}@${host}`);
      if (!targetUri) return;
      await currentSession.refer(targetUri, {
        requestDelegate: {
          onAccept: () => log('blind-transfer-accepted'),
          onReject: () => log('blind-transfer-rejected')
        }
      });
    } catch (e) {
      logErr('blind-transfer-failed', e);
    }
  }

  async function attendedTransferStart(target) {
    if (!currentSession || currentSession.state !== SIP.SessionState.Established) return;
    try {
      await setHold(true);
      const host = extractHost(settings.wssUrl);
      const targetUri = SIP.UserAgent.makeURI(`sip:${target}@${host}`);
      if (!targetUri) return;

      const inviter = new SIP.Inviter(userAgent, targetUri, {
        sessionDescriptionHandlerOptions: { constraints: mediaConstraints() }
      });
      attendedTransferSession = inviter;

      inviter.stateChange.addListener((state) => {
        if (state === SIP.SessionState.Terminated && attendedTransferSession === inviter) {
          attendedTransferSession = null;
        }
      });

      await inviter.invite();
    } catch (e) {
      logErr('attended-transfer-start-failed', e);
      attendedTransferSession = null;
    }
  }

  async function attendedTransferComplete() {
    if (!currentSession || !attendedTransferSession) return;
    try {
      await currentSession.refer(attendedTransferSession, {
        requestDelegate: {
          onAccept: () => log('attended-transfer-completed'),
          onReject: () => log('attended-transfer-refer-rejected')
        }
      });
      try {
        await attendedTransferSession.bye();
      } catch (e) {
        /* second leg may already be replaced/terminated by the far end */
      }
    } catch (e) {
      logErr('attended-transfer-complete-failed', e);
    } finally {
      attendedTransferSession = null;
      await hangup();
    }
  }

  async function attendedTransferCancel() {
    if (attendedTransferSession) {
      try {
        const state = attendedTransferSession.state;
        if (state === SIP.SessionState.Established) {
          await attendedTransferSession.bye();
        } else {
          await attendedTransferSession.cancel();
        }
      } catch (e) {
        logErr('attended-transfer-cancel-failed', e);
      }
      attendedTransferSession = null;
    }
    await setHold(false);
  }

  // ---------------------------------------------------------------------
  // Call history (store lives in main)
  // ---------------------------------------------------------------------
  async function addHistoryEntry(entry) {
    try { await window.centinelo.addHistory(entry); } catch (e) { /* best-effort */ }
  }

  async function updateLastHistoryDuration(durationMs) {
    try { await window.centinelo.setLastHistoryDuration(Math.round(durationMs / 1000)); } catch (e) { /* best-effort */ }
  }

  // ---------------------------------------------------------------------
  // Devices
  // ---------------------------------------------------------------------
  async function listDevices() {
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      return devices
        .filter((d) => d.kind === 'audioinput' || d.kind === 'audiooutput')
        .map((d) => ({ deviceId: d.deviceId, kind: d.kind, label: d.label || d.kind }));
    } catch (e) {
      logErr('list-devices-failed', e);
      return [];
    }
  }

  async function applyDeviceSettings(devices) {
    if (devices.micDeviceId) settings.micDeviceId = devices.micDeviceId;
    if (devices.speakerDeviceId) settings.speakerDeviceId = devices.speakerDeviceId;
    if (devices.ringerDeviceId) settings.ringerDeviceId = devices.ringerDeviceId;

    if (remoteAudioEl.srcObject) {
      await applySinkId(remoteAudioEl, settings.speakerDeviceId);
    }
    await applySinkId(ringtoneAudioEl, settings.ringerDeviceId);

    // Swap outbound mic track live if in a call.
    if (currentSession && currentSession.state === SIP.SessionState.Established) {
      try {
        const sdh = currentSession.sessionDescriptionHandler;
        const pc = sdh && sdh.peerConnection;
        if (pc) {
          const newStream = await navigator.mediaDevices.getUserMedia(mediaConstraints());
          const newTrack = newStream.getAudioTracks()[0];
          const sender = pc.getSenders().find((s) => s.track && s.track.kind === 'audio');
          if (sender && newTrack) {
            await sender.replaceTrack(newTrack);
            newTrack.enabled = !isMuted;
          }
        }
      } catch (e) {
        logErr('swap-mic-device-failed', e);
      }
    }
  }

  async function requestMicPermission() {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      stream.getTracks().forEach((t) => t.stop());
      return { granted: true };
    } catch (e) {
      return { granted: false, error: String(e && e.name) };
    }
  }

  // ---------------------------------------------------------------------
  // Hotkeys + dial requests pushed from main (protocol / bridge / clipboard)
  // ---------------------------------------------------------------------
  window.centinelo.onHotkey((action) => {
    if (action === 'answer') {
      if (waitingInvitation && callState === STATE.IN_CALL) answerWaiting();
      else answerCall();
    }
    if (action === 'hangup') {
      if (callState === STATE.RINGING) rejectCall();
      else hangup();
    }
  });

  window.centinelo.onDialRequest((number) => {
    if (callState === STATE.REGISTERED) {
      dial(number);
    } else {
      // Not ready / already on a call: pre-fill the dial input instead.
      stateListeners.forEach((fn) => {
        try { fn({ ...snapshotState(), prefillNumber: number }); } catch (e) { /* noop */ }
      });
    }
  });

  // ---------------------------------------------------------------------
  // Public API
  // ---------------------------------------------------------------------
  self.Engine = {
    onState(fn) { stateListeners.add(fn); fn(snapshotState()); },
    getState: snapshotState,
    dial,
    answer: answerCall,
    reject: rejectCall,
    hangup,
    mute: () => setMute(true),
    unmute: () => setMute(false),
    hold: () => setHold(true),
    unhold: () => setHold(false),
    dtmf: sendDtmf,
    blindTransfer,
    attendedTransferStart,
    attendedTransferComplete,
    attendedTransferCancel,
    answerWaiting,
    rejectWaiting,
    swapCalls,
    reconnect: async () => { reconnectAttempt = 0; await startEngine(); },
    listDevices,
    applyDeviceSettings,
    requestMicPermission
  };

  // ---------------------------------------------------------------------
  // Boot
  // ---------------------------------------------------------------------
  startEngine();
})();

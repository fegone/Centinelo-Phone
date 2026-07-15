/**
 * Centinelo Phone — shared constants and helpers (renderer).
 * Shared constants ported from the original extension engine; storage lives in main.
 */
(function (root) {
  'use strict';

  const CentineloCommon = {
    APP_VERSION: '1.0.0', // keep in sync with package.json (used in SIP User-Agent)

    MAX_FAVORITES: 4,

    BLF_STATE: {
      IDLE: 'idle',
      RINGING: 'ringing',
      BUSY: 'busy',
      UNKNOWN: 'unknown'
    },

    CALL_STATE: {
      DISCONNECTED: 'disconnected',
      CONNECTING: 'connecting',
      REGISTERED: 'registered',
      RINGING: 'ringing',
      CALLING: 'calling',
      IN_CALL: 'in-call',
      HELD: 'held'
    },

    DIRECTION: {
      INBOUND: 'inbound',
      OUTBOUND: 'outbound'
    },

    formatDuration(totalSeconds) {
      const s = Math.max(0, Math.floor(totalSeconds));
      const hrs = Math.floor(s / 3600);
      const mins = Math.floor((s % 3600) / 60);
      const secs = s % 60;
      const mm = String(mins).padStart(2, '0');
      const ss = String(secs).padStart(2, '0');
      if (hrs > 0) return `${hrs}:${mm}:${ss}`;
      return `${mm}:${ss}`;
    },

    normalizeNumber(num) {
      if (!num) return '';
      let digits = String(num).replace(/[^\d+]/g, '');
      digits = digits.replace(/^\+?1(\d{10})$/, '$1');
      return digits;
    },

    formatUSNumber(num) {
      const digits = this.normalizeNumber(num).replace(/\D/g, '');
      if (digits.length === 10) {
        return `(${digits.slice(0, 3)}) ${digits.slice(3, 6)}-${digits.slice(6)}`;
      }
      return num;
    },

    logEvent(debugEnabled, eventName, extra) {
      const prefix = '[Centinelo]';
      if (debugEnabled) {
        console.log(prefix, eventName, extra || '');
      } else {
        console.log(prefix, eventName);
      }
    },

    logError(debugEnabled, eventName, err) {
      const prefix = '[Centinelo]';
      if (debugEnabled && err) {
        console.error(prefix, eventName, err);
      } else {
        console.error(prefix, eventName);
      }
    },

    PHONE_REGEX: /(?<![\d.\-/])(?:\+?1[\s.-]?)?\(?\d{3}\)?[\s.-]?\d{3}[\s.-]?\d{4}(?![\d])/g
  };

  root.CentineloCommon = CentineloCommon;
})(typeof self !== 'undefined' ? self : this);

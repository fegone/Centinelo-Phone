/**
 * Centinelo Click-to-Call — content script.
 * Slim click-to-call detection: wraps US phone numbers
 * and tel: links so a click dials in the Centinelo Phone desktop app.
 * No banner, no audio — the native app owns all call UX.
 */
(function () {
  'use strict';

  // US phone detection regex (word-boundary guarded).
  const PHONE_REGEX = /(?<![\d.\-/])(?:\+?1[\s.-]?)?\(?\d{3}\)?[\s.-]?\d{3}[\s.-]?\d{4}(?![\d])/g;
  const WRAP_CLASS = 'centinelo-c2c';
  const SKIP_TAGS = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEXTAREA', 'INPUT', 'SELECT', 'A', 'BUTTON']);

  function dial(number) {
    const clean = String(number || '').replace(/[^\d+*#]/g, '');
    if (clean.length < 7) return;
    chrome.runtime.sendMessage({ cmd: 'dial', number: clean }, (resp) => {
      if (chrome.runtime.lastError) return;
      if (resp && resp.error === 'no-token') {
        console.warn('[Centinelo] Set the bridge token in the extension options.');
      }
    });
  }

  // tel: links — intercept and route to the app.
  document.addEventListener('click', (e) => {
    const a = e.target && e.target.closest && e.target.closest('a[href^="tel:"]');
    if (a) {
      e.preventDefault();
      e.stopPropagation();
      dial(decodeURIComponent(a.getAttribute('href').slice(4)));
    }
  }, true);

  // Free-text numbers — wrap in clickable spans.
  function wrapTextNode(node) {
    const text = node.nodeValue;
    if (!text || text.length < 10) return;
    PHONE_REGEX.lastIndex = 0;
    if (!PHONE_REGEX.test(text)) return;
    PHONE_REGEX.lastIndex = 0;

    const frag = document.createDocumentFragment();
    let last = 0;
    let m;
    while ((m = PHONE_REGEX.exec(text)) !== null) {
      const start = m.index;
      if (start > last) frag.appendChild(document.createTextNode(text.slice(last, start)));
      const span = document.createElement('span');
      span.className = WRAP_CLASS;
      span.textContent = m[0];
      span.title = 'Call with Centinelo Phone';
      span.style.cssText = 'cursor:pointer;text-decoration:underline dotted;text-underline-offset:2px;';
      span.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        dial(m[0]);
      });
      frag.appendChild(span);
      last = start + m[0].length;
    }
    if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
    node.parentNode.replaceChild(frag, node);
  }

  function scan(root) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        const p = node.parentElement;
        if (!p || SKIP_TAGS.has(p.tagName) || p.closest('.' + WRAP_CLASS) || p.isContentEditable) {
          return NodeFilter.FILTER_REJECT;
        }
        return NodeFilter.FILTER_ACCEPT;
      }
    });
    const nodes = [];
    while (walker.nextNode()) nodes.push(walker.currentNode);
    nodes.forEach(wrapTextNode);
  }

  let scanTimer = null;
  const observer = new MutationObserver(() => {
    if (scanTimer) return;
    scanTimer = setTimeout(() => {
      scanTimer = null;
      scan(document.body);
    }, 1500);
  });

  if (document.body) {
    scan(document.body);
    observer.observe(document.body, { childList: true, subtree: true });
  }
})();

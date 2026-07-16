// Tests for i18n.js (2026-07-16 4R re-review, RELIABILITY A3 - this
// 556-line module had zero coverage of its own despite being imported by
// both app.js and transcript-panel.js). Node's built-in test runner
// (`node:test`/`node:assert`), same "no new devDependency" style as
// transcript-panel.test.js. Run: `npm test` (from shell/) or
// `node --test ui/js/*.test.js`.
//
// i18n.js's active locale is module-level mutable state, shared by every
// test in this process that imports it (including transcript-panel.js's
// own import, if node runs multiple test files in one process) - every
// test here restores "en" in its own `finally`/at file end via `after()`,
// so this file never leaks a non-default locale into whatever test runs
// next, regardless of execution order or process-sharing.

import { test, after } from "node:test";
import assert from "node:assert/strict";
import { t, setLocale, getLocale, getLocalePref, detectSystemLocale, localeTag, SUPPORTED_LOCALES } from "./i18n.js";

after(() => {
  setLocale("en");
});

// ---------------------------------------------------------------------
// setLocale / getLocale / getLocalePref - the "auto" resolution contract
// (mirrors ThemePref::Auto's own semantic, see settings.rs LocalePref doc).
// ---------------------------------------------------------------------

test("setLocale: an explicit supported locale resolves to itself", () => {
  assert.equal(setLocale("pt-BR"), "pt-BR");
  assert.equal(getLocale(), "pt-BR");
  assert.equal(getLocalePref(), "pt-BR");
  assert.equal(setLocale("es"), "es");
  assert.equal(getLocale(), "es");
  setLocale("en");
});

test("setLocale: an unsupported/garbage value falls back to auto-resolution, not a throw", () => {
  const resolved = setLocale("klingon");
  assert.ok(SUPPORTED_LOCALES.includes(resolved), `expected a supported locale, got ${resolved}`);
  assert.equal(getLocalePref(), "auto");
  setLocale("en");
});

test("setLocale('auto'): resolves via detectSystemLocale, never stays the literal string 'auto'", () => {
  const resolved = setLocale("auto");
  assert.ok(SUPPORTED_LOCALES.includes(resolved));
  assert.equal(getLocalePref(), "auto");
  setLocale("en");
});

test("detectSystemLocale: never throws when navigator/navigator.language is absent (plain non-browser runtime, e.g. an older Node without the Web-standard `navigator` global)", () => {
  // Newer Node (>=21ish, this repo's test runtime included) defines a
  // global `navigator` with a real `.language` - so this can't assert
  // "navigator is undefined" as the environment fact; instead it proves
  // the function's own defensive guard directly by simulating the
  // browser-less shape (no navigator at all), which is what
  // transcript-panel.test.js's import chain would hit on an older
  // runtime. Doesn't touch the real global - only the value this one
  // call sees.
  const real = globalThis.navigator;
  try {
    // Can't delete a getter-backed global in some Node builds; overriding
    // the property value (not deleting it) is portable either way.
    Object.defineProperty(globalThis, "navigator", { value: undefined, configurable: true });
    assert.doesNotThrow(() => detectSystemLocale());
    assert.equal(detectSystemLocale(), "en");
  } finally {
    Object.defineProperty(globalThis, "navigator", { value: real, configurable: true });
  }
});

test("detectSystemLocale: always returns a supported locale in THIS runtime's actual environment, whatever navigator.language happens to be", () => {
  const detected = detectSystemLocale();
  assert.ok(SUPPORTED_LOCALES.includes(detected), `expected a supported locale, got ${detected}`);
});

test("detectSystemLocale: maps pt*/es*/en*/unrecognized language tags to pt-BR/es/en/en respectively", () => {
  const real = globalThis.navigator;
  const cases = [
    [["pt-PT"], "pt-BR"], // only Portuguese variant this product ships (task brief)
    [["pt-BR"], "pt-BR"],
    [["es-ES"], "es"],
    [["es-419"], "es"],
    [["en-GB"], "en"],
    [["fr-FR"], "en"], // unsupported language -> en fallback
    [[], "en"], // empty languages list -> en fallback
  ];
  try {
    for (const [languages, expected] of cases) {
      Object.defineProperty(globalThis, "navigator", { value: { languages, language: languages[0] }, configurable: true });
      assert.equal(detectSystemLocale(), expected, `languages=${JSON.stringify(languages)}`);
    }
  } finally {
    Object.defineProperty(globalThis, "navigator", { value: real, configurable: true });
  }
});

// ---------------------------------------------------------------------
// t() - key resolution, per-locale values, missing-key fallback chain,
// {var} interpolation.
// ---------------------------------------------------------------------

test("t(): resolves a known key against the active locale", () => {
  setLocale("en");
  assert.equal(t("settings.save"), "Save");
  setLocale("pt-BR");
  assert.equal(t("settings.save"), "Salvar");
  setLocale("es");
  assert.equal(t("settings.save"), "Guardar");
  setLocale("en");
});

test("t(): every locale actually differs for a longer, clearly-translated string (not just copy-pasted English)", () => {
  setLocale("en");
  const en = t("settings.favoritesHint");
  setLocale("pt-BR");
  const ptBr = t("settings.favoritesHint");
  setLocale("es");
  const es = t("settings.favoritesHint");
  setLocale("en");
  assert.notEqual(en, ptBr);
  assert.notEqual(en, es);
  assert.notEqual(ptBr, es);
});

test("t(): interpolates a single {var} token", () => {
  setLocale("en");
  assert.equal(t("favorites.extFallback", { ext: "1042" }), "Ext 1042");
});

test("t(): interpolates multiple distinct {var} tokens", () => {
  setLocale("en");
  assert.equal(t("titlebarState.reconnecting", { attempt: 2, max: 5 }), "Reconnecting the phone engine… (2/5)");
});

test("t(): a var value is substituted literally, even if it happens to contain another var's token syntax (single-pass over the original string, not recursive)", () => {
  // 4R re-review, LOW finding: t() used to loop split/join per var
  // sequentially, so a var's *value* containing literal "{otherVarName}"
  // text got re-substituted by a LATER iteration - e.g. this exact case
  // used to resolve to "Dialing the bridge from the bridge." because the
  // "{source}" text injected for {number} got matched again once the
  // "source" iteration ran. The fix computes every replacement from the
  // ORIGINAL string in one combined-regex pass, so injected text is never
  // rescanned - {number}'s "{source}" value survives untouched here.
  setLocale("en");
  const result = t("call.dialingFrom", { number: "{source}", source: "the bridge" });
  assert.equal(result, "Dialing {source} from the bridge.");
});

test("t(): a key missing from the active locale falls back to the English value", () => {
  setLocale("pt-BR");
  // Every real key has all 3 locales populated (see the ENTRIES table),
  // so this simulates the fallback path directly rather than depending on
  // an actual gap existing in the dictionary today.
  assert.equal(t("__no_such_key__"), "__no_such_key__", "sanity: unknown key anywhere falls back to itself");
  setLocale("en");
});

test("t(): a completely unknown key returns the key itself, not undefined/blank - visibly broken beats silently blank", () => {
  setLocale("en");
  assert.equal(t("this.key.does.not.exist"), "this.key.does.not.exist");
});

test("t(): vars object with no matching {token} in the string is a no-op, not a throw", () => {
  setLocale("en");
  assert.equal(t("settings.save", { unused: "value" }), "Save");
});

// ---------------------------------------------------------------------
// localeTag() - BCP-47 tag handed to Intl/Date formatting (app.js
// fmtClock/fmtWhen, transcript-panel.js fmtClock/fmtDate).
// ---------------------------------------------------------------------

test("localeTag(): maps each locale to its Intl-facing BCP-47 tag", () => {
  setLocale("en");
  assert.equal(localeTag(), "en-US");
  setLocale("pt-BR");
  assert.equal(localeTag(), "pt-BR");
  setLocale("es");
  // es-419 (neutral Latin American Spanish), not bare "es" (which Intl
  // would treat as Spain) - task brief: "ES neutro-latino".
  assert.equal(localeTag(), "es-419");
  setLocale("en");
});

test("localeTag(): is always a valid tag Intl.DateTimeFormat accepts, for every supported locale", () => {
  for (const locale of SUPPORTED_LOCALES) {
    setLocale(locale);
    assert.doesNotThrow(() => new Intl.DateTimeFormat(localeTag()), `localeTag() for ${locale} should be Intl-valid`);
  }
  setLocale("en");
});

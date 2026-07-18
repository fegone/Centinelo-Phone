// node:test coverage for blf-ui.js's pure visibility rule (P5, "BLF favorites
// admin toggle"). Mirrors updater.test.js / i18n.test.js's pure-function
// convention — no DOM harness; the rule itself is what's locked, app.js's
// applier (renderBlfUi) is verified visually like the other renderers here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { computeBlfUiHidden, BLF_UI_TARGETS } from "./blf-ui.js";

// The contract the DOM applier in app.js relies on: the exact ids it toggles.
test("BLF_UI_TARGETS: pins the DOM ids the applier toggles", () => {
  assert.equal(BLF_UI_TARGETS.favoritesHeading, "favorites-main-heading");
  assert.equal(BLF_UI_TARGETS.favoritesGrid, "favorites-grid");
  assert.equal(BLF_UI_TARGETS.console, "btn-console");
});

// Floor test (3): with the master switch OFF, the favorites grid + heading
// disappear from view (absent, not greyed).
test("blf disabled: favorites grid + heading are hidden", () => {
  const hidden = computeBlfUiHidden({ blfEnabled: false, consoleUnlocked: true });
  assert.equal(hidden.favoritesGrid, true, "grid must be hidden");
  assert.equal(hidden.favoritesHeading, true, "heading must be hidden");
});

// Floor test (3): with the master switch OFF, the premium console entry point
// disappears TOO — even when its own license gate would otherwise clear it.
// This is the whole point of the feature: BLF off = no console surface at all.
test("blf disabled: console hidden even when the license gate cleared", () => {
  const hidden = computeBlfUiHidden({ blfEnabled: false, consoleUnlocked: true });
  assert.equal(hidden.console, true, "console must be hidden when BLF is off");
});

// Shipped behavior unchanged: master switch ON keeps the favorites grid +
// heading visible.
test("blf enabled: favorites grid + heading shown", () => {
  const hidden = computeBlfUiHidden({ blfEnabled: true, consoleUnlocked: false });
  assert.equal(hidden.favoritesGrid, false);
  assert.equal(hidden.favoritesHeading, false);
});

// When BLF is on, the console follows ONLY its own license gate (the master
// switch stops overriding it).
test("blf enabled: console follows its own license gate", () => {
  assert.equal(computeBlfUiHidden({ blfEnabled: true, consoleUnlocked: true }).console, false);
  assert.equal(computeBlfUiHidden({ blfEnabled: true, consoleUnlocked: false }).console, true);
});

// No-arg call models the cold-boot window before the settings/premium fetches
// resolve: must look like today's shipped app (BLF on, console locked until
// premium is checked).
test("defaults: blf on + console locked, the shipped cold-boot appearance", () => {
  const hidden = computeBlfUiHidden();
  assert.equal(hidden.favoritesGrid, false);
  assert.equal(hidden.favoritesHeading, false);
  assert.equal(hidden.console, true);
});

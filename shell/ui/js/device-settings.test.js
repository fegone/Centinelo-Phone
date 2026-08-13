import { test } from "node:test";
import assert from "node:assert/strict";
import {
  friendlyDeviceName,
  buildDeviceOptions,
  buildSaveDeviceInput,
  renderDeviceOptionsHtml,
  activeDeviceLabel,
} from "./device-settings.js";

// ---- friendlyDeviceName ----------------------------------------------------

test("friendlyDeviceName: strips the module prefix off a real per-device name", () => {
  assert.equal(friendlyDeviceName("coreaudio,MacBook Pro Microphone"), "MacBook Pro Microphone");
  assert.equal(friendlyDeviceName("wasapi,USB Headset (2- Jabra)"), "USB Headset (2- Jabra)");
});

test("friendlyDeviceName: a module-only fallback name (no comma) is returned as-is", () => {
  // devices_add_driver()'s "no real per-device enumeration" fallback shape.
  assert.equal(friendlyDeviceName("ausine"), "ausine");
});

test("friendlyDeviceName: never invents copy for an empty/missing name", () => {
  assert.equal(friendlyDeviceName(""), "");
  assert.equal(friendlyDeviceName(undefined), "");
});

// ---- buildDeviceOptions -----------------------------------------------------

test("buildDeviceOptions: always prepends the UI-only System default row first", () => {
  const { options } = buildDeviceOptions([]);
  assert.equal(options.length, 1);
  assert.equal(options[0].id, "");
});

test("buildDeviceOptions: maps the engine's real entries after System default, using their friendly name", () => {
  const { options } = buildDeviceOptions([
    { name: "coreaudio,Headset Mic", active: false },
    { name: "coreaudio,Built-in Microphone", active: false },
  ]);
  assert.deepEqual(
    options.map((o) => o.id),
    ["", "coreaudio,Headset Mic", "coreaudio,Built-in Microphone"]
  );
  assert.equal(options[1].name, "Headset Mic");
});

test("buildDeviceOptions: activeId is the engine's own flagged entry", () => {
  const { activeId } = buildDeviceOptions([
    { name: "coreaudio,Headset Mic", active: false },
    { name: "coreaudio,Built-in Microphone", active: true },
  ]);
  assert.equal(activeId, "coreaudio,Built-in Microphone");
});

test("buildDeviceOptions: no entry flagged active resolves to System default, not an unmatched id", () => {
  // The real, expected shape whenever nothing has ever been explicitly
  // chosen - resolve_device's cfg_dev is the literal string "default",
  // which no real hardware device is ever named (devices_add_driver's own
  // doc/cross-check).
  const { activeId } = buildDeviceOptions([
    { name: "coreaudio,Headset Mic", active: false },
    { name: "coreaudio,Built-in Microphone", active: false },
  ]);
  assert.equal(activeId, "");
});

test("buildDeviceOptions: an empty engine list still yields just the System default row", () => {
  const { options, activeId } = buildDeviceOptions([]);
  assert.equal(options.length, 1);
  assert.equal(options[0].id, "");
  assert.equal(activeId, "");
});

// ---- buildSaveDeviceInput ---------------------------------------------------

test("buildSaveDeviceInput: picks input_device/output_device by kind", () => {
  assert.deepEqual(buildSaveDeviceInput("input", "coreaudio,Headset Mic"), { input_device: "coreaudio,Headset Mic" });
  assert.deepEqual(buildSaveDeviceInput("output", "coreaudio,Headset Mic"), { output_device: "coreaudio,Headset Mic" });
});

test("buildSaveDeviceInput: System default round-trips as the empty-string clear sentinel", () => {
  assert.deepEqual(buildSaveDeviceInput("input", ""), { input_device: "" });
  assert.deepEqual(buildSaveDeviceInput("output", ""), { output_device: "" });
});

// ---- renderDeviceOptionsHtml / activeDeviceLabel ---------------------------

test("renderDeviceOptionsHtml: marks exactly the active option aria-selected", () => {
  const { options } = buildDeviceOptions([{ name: "coreaudio,Headset Mic", active: true }]);
  const html = renderDeviceOptionsHtml(options, "coreaudio,Headset Mic");
  const selectedCount = (html.match(/aria-selected="true"/g) || []).length;
  assert.equal(selectedCount, 1);
  assert.ok(html.includes('data-device-id="coreaudio,Headset Mic"'));
});

test("renderDeviceOptionsHtml: escapes a device name that contains markup-like characters", () => {
  const { options } = buildDeviceOptions([{ name: 'coreaudio,<script>alert(1)</script>', active: false }]);
  const html = renderDeviceOptionsHtml(options, "");
  assert.ok(!html.includes("<script>"));
});

test("activeDeviceLabel: returns the matching option's own name", () => {
  const { options } = buildDeviceOptions([{ name: "coreaudio,Headset Mic", active: true }]);
  assert.equal(activeDeviceLabel(options, "coreaudio,Headset Mic"), "Headset Mic");
});

test("activeDeviceLabel: falls back to System default's own name for an unmatched id", () => {
  const { options } = buildDeviceOptions([{ name: "coreaudio,Headset Mic", active: false }]);
  assert.equal(activeDeviceLabel(options, "coreaudio,Nonexistent"), options[0].name);
});

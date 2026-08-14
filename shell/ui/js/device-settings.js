// Settings → Audio & devices → Microphone & speaker (Plate 10,
// premium/design/mockups/settings-devices.html, PR #10 design/settings-devices).
//
// Pure state/render logic, zero Tauri dependency - same "testable without a
// live runtime" convention codec-settings.js/transcript-panel.js/... already
// document for themselves. app.js owns the invoke/listen wiring (querying
// `sidecar_list_devices`, calling `save_audio_settings`, reacting to the
// `devices` sidecar event) and hands this module plain data; this file never
// touches `document`, `invoke`, or a module-level mutable singleton.
//
// **Fuente de verdad (this workspace's own rule)**: the SET of devices, and
// which one is active, ALWAYS comes straight from the engine's own
// `core/PROTOCOL.md` v1.1 `devices` event (`input`/`output`, each
// `[{name,active},...]`) via `buildDeviceOptions` below - never a list
// hardcoded in this UI. The one thing this UI adds on top, deliberately -
// see design/notes/settings-devices.md "System default es fila de la UI, no
// del motor" - is the "System default" row itself (`id: ""`), which maps to
// `commands::SaveAudioInput`'s own "explicit clear back to the platform
// default" sentinel (`Some("")`/all-whitespace, see `merge_device_choice`'s
// doc in commands.rs): `buildSaveDeviceInput` round-trips that same `""`
// straight to the backend, no separate "is this the default row" branch
// needed anywhere in app.js.
//
// **No admin lock** - this panel intentionally sits in the SAME free-tier
// zone as codec-settings.js (design/notes/settings-devices.md "Dónde vive y
// para quién", explicitly mirroring Plate 09's own approved reasoning): the
// real scenario is a receptionist being walked through "open Settings, pick
// your headset" by phone support with no admin password in hand, and picking
// the wrong device exposes no secret/patient data - the level meter/loopback
// test (out of scope for this pass, see this workspace's shell-tauri report)
// diagnoses a wrong pick in seconds on the same screen.
//
// **No apply bar - selecting IS applying.** Unlike codecs (`set_codecs`,
// "next call"), `core/PROTOCOL.md`'s `set_device` hot-swaps a call already in
// progress via baresip's `audio_set_source()`/`audio_set_player()` AND
// persists for the next call, in one shot - so there is deliberately no
// discard/apply/dirty-tracking state in this module at all, unlike
// codec-settings.js's `isDirty`/`applyToggle`/`applyMove`. Selecting a row
// calls `save_audio_settings` immediately (see app.js `wireDeviceHandlers`).

import { t } from "./i18n.js";
import { escapeHtml, escapeAttr } from "./dom-utils.js";

/// Splits `core/PROTOCOL.md`'s `"<module>[,<device>]"` device-name shape
/// into the human-readable part a real per-device backend (`coreaudio`/
/// `wasapi`) already provides - never invented copy, just the same string
/// the engine sent back, minus its own module prefix. A name with no comma
/// (the module-only fallback `devices_add_driver()` emits when this
/// particular build/platform has no real per-device enumeration - see that
/// function's own header comment) falls back to the raw module name itself,
/// still real data, just less specific.
export function friendlyDeviceName(name) {
  const s = String(name || "");
  const i = s.indexOf(",");
  return i === -1 ? s : s.slice(i + 1);
}

/// Builds this panel's option list straight from the engine's own `devices`
/// event's `input`/`output` array (`[{name,active},...]`, `core/PROTOCOL.md`
/// v1.1). Prepends the UI-only "System default" row (`id: ""`) - see this
/// file's header comment. `activeId` is the engine's own currently-active
/// entry if one is flagged `active:true`, or `""` (System default) when none
/// is - which is also the real, expected shape whenever nothing has ever
/// been explicitly chosen (`resolve_device`'s `cfg_dev` is the literal
/// string `"default"`, which a real hardware device is never named, so
/// `devices_add_driver()`'s own `active` cross-check leaves every real entry
/// `false` in that case - see commands.rs `apply_live_device`'s doc for the
/// same convention on the persist side).
export function buildDeviceOptions(list) {
  const entries = Array.isArray(list) ? list : [];
  const options = [
    { id: "", name: t("settings.deviceSystemDefault"), hint: t("settings.deviceSystemDefaultHint") },
    ...entries.map((d) => ({ id: String(d.name || ""), name: friendlyDeviceName(d.name), hint: null })),
  ];
  const activeEntry = entries.find((d) => d.active === true);
  const activeId = activeEntry ? String(activeEntry.name || "") : "";
  return { options, activeId };
}

/// The exact payload `save_audio_settings` expects for ONE direction
/// (`commands::SaveAudioInput` - `{ input_device: ... }` or
/// `{ output_device: ... }`, snake_case field, matches
/// `#[tauri::command(rename_all = "snake_case")]`). `id === ""` (the System
/// default row) round-trips as `""` unmodified - `merge_device_choice`'s own
/// doc documents that exact string as its "explicit clear" sentinel, so this
/// function does no translation, just picks the right field name for
/// `kind`.
export function buildSaveDeviceInput(kind, id) {
  return kind === "output" ? { output_device: id } : { input_device: id };
}

const CHECK_SVG =
  '<svg class="check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>';

/// Renders one `.pop`'s (the custom listbox popup - mockup's `#mic-pop`/
/// `#spk-pop`) inner HTML from `buildDeviceOptions`'s own `options`/
/// `activeId` - the ONLY place option markup is built, called both on open
/// and after every selection (mirrors `codec-settings.js`'s
/// `renderCodecsList` "rebuild wholesale" convention, no incremental DOM
/// patching).
export function renderDeviceOptionsHtml(options, activeId) {
  return options
    .map((o) => {
      const selected = o.id === activeId;
      const idAttr = escapeAttr(o.id);
      const hint = o.hint ? `<span>${escapeHtml(o.hint)}</span>` : "";
      return `<button type="button" class="opt" role="option" data-device-id="${idAttr}" aria-selected="${selected}">
        ${CHECK_SVG}
        <span class="ox"><b>${escapeHtml(o.name)}</b>${hint}</span>
      </button>`;
    })
    .join("");
}

/// The button label (mockup's `#mic-selname`/`#spk-selname`) for whichever
/// option's `id === activeId` - falls back to the System default row's own
/// name (`options[0]`, always present - see `buildDeviceOptions`) if
/// `activeId` somehow doesn't match anything, rather than rendering blank.
export function activeDeviceLabel(options, activeId) {
  return (options.find((o) => o.id === activeId) || options[0]).name;
}

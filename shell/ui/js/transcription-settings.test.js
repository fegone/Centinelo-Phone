// Tests for transcription-settings.js (2026-07-17 4R re-review,
// RELIABILITY #1 - the Plate 08 settings pane shipped with zero coverage
// of its own logic; the 48 JS tests that existed before this file were all
// preexisting i18n/dom-utils/transcript-panel suites). Node's built-in
// test runner, same style as this directory's other *.test.js files.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  MODEL_CLI_TIER_TO_SETTINGS_TIER,
  mapCliTierToSettingsTier,
  buildSaveTranscriptionInput,
  formatModelSize,
  nextGeneration,
  startDownload,
  applyProgressEvent,
  isDownloadStalled,
  computeDownloadPct,
  computeModelChipState,
} from "./transcription-settings.js";

// ---------------------------------------------------------------------
// (a) MODEL_CLI_TIER_TO_SETTINGS_TIER - pinned against transcription.rs's
// own tier_cli_name() outputs (src-tauri/src/transcription.rs's
// tier_cli_names_match_the_real_binarys_parse_function test pins the Rust
// side of this same mapping - large-v3-turbo-q5_0/small-q5_1 are not
// arbitrary strings, they're the real whisper.cpp model tier names the
// `transcription://model-download-*` events actually carry).
// ---------------------------------------------------------------------

test("mapCliTierToSettingsTier: maps the real accurate-tier CLI name", () => {
  assert.equal(mapCliTierToSettingsTier("large-v3-turbo-q5_0"), "accurate");
});

test("mapCliTierToSettingsTier: maps the real light-tier CLI name", () => {
  assert.equal(mapCliTierToSettingsTier("small-q5_1"), "light");
});

test("mapCliTierToSettingsTier: an unrecognized CLI tier name maps to null, not a throw or a guess", () => {
  assert.equal(mapCliTierToSettingsTier("some-future-model-tag"), null);
  assert.equal(mapCliTierToSettingsTier(""), null);
  assert.equal(mapCliTierToSettingsTier(undefined), null);
});

test("MODEL_CLI_TIER_TO_SETTINGS_TIER: exactly the two tiers this app ships, no more", () => {
  assert.deepEqual(Object.keys(MODEL_CLI_TIER_TO_SETTINGS_TIER).sort(), ["large-v3-turbo-q5_0", "small-q5_1"].sort());
});

// ---------------------------------------------------------------------
// (b) buildSaveTranscriptionInput - the exact payload
// save_transcription_settings expects (commands::SaveTranscriptionInput:
// mode, activation, keep_audio, storage_dir, view_only, model_tier,
// language - all 7 fields, snake_case).
// ---------------------------------------------------------------------

test("buildSaveTranscriptionInput: maps all 7 fields to the backend's exact snake_case shape", () => {
  const input = buildSaveTranscriptionInput({
    mode: "live",
    activation: "manual",
    keepAudio: true,
    storageDir: "//archive/front-desk/calls",
    viewOnly: false,
    modelTier: "light",
    language: "es",
  });
  assert.deepEqual(input, {
    mode: "live",
    activation: "manual",
    keep_audio: true,
    storage_dir: "//archive/front-desk/calls",
    view_only: false,
    model_tier: "light",
    language: "es",
  });
});

test("buildSaveTranscriptionInput: trims storage_dir the same way #in-core-path's own save trims its path", () => {
  const input = buildSaveTranscriptionInput({
    mode: "off",
    activation: "all_calls",
    keepAudio: false,
    storageDir: "  //archive/front-desk/calls  ",
    viewOnly: false,
    modelTier: "accurate",
    language: "auto",
  });
  assert.equal(input.storage_dir, "//archive/front-desk/calls");
});

test("buildSaveTranscriptionInput: coerces keepAudio/viewOnly to real booleans, not truthy passthrough", () => {
  const input = buildSaveTranscriptionInput({
    mode: "off",
    activation: "all_calls",
    keepAudio: undefined,
    storageDir: "",
    viewOnly: undefined,
    modelTier: "accurate",
    language: "auto",
  });
  assert.equal(input.keep_audio, false);
  assert.equal(input.view_only, false);
  assert.equal(typeof input.keep_audio, "boolean");
  assert.equal(typeof input.view_only, "boolean");
});

test("buildSaveTranscriptionInput: a missing/empty storageDir becomes an empty string, not null/undefined", () => {
  const input = buildSaveTranscriptionInput({
    mode: "off",
    activation: "all_calls",
    keepAudio: false,
    storageDir: undefined,
    viewOnly: false,
    modelTier: "accurate",
    language: "auto",
  });
  assert.equal(input.storage_dir, "");
});

// ---------------------------------------------------------------------
// (c) model status chip state machine - the two transitions the 4R
// re-review called out by name: downloading -> done -> installed, and
// downloading -> error -> back to offering Download.
// ---------------------------------------------------------------------

test("chip state machine: downloading -> done -> installed", () => {
  // 1. a download starts
  let download = startDownload(0, 1000);
  let status = null; // not fetched yet this session
  assert.deepEqual(computeModelChipState(download, status), { kind: "downloading", pct: null });

  // 2. a progress event lands
  download = applyProgressEvent(download, { downloaded_bytes: 250_000_000, total_bytes: 500_000_000 }, 2000);
  assert.deepEqual(computeModelChipState(download, status), { kind: "downloading", pct: 50 });

  // 3. transcription://model-download-done settles it - app.js's
  // handleModelDownloadSettled clears the download record to null and
  // refreshModelStatuses() re-fetches real disk state (never inferred
  // from the done event itself - see that function's own doc).
  download = null;
  status = { present: true, sizeBytes: 500_000_000 };
  assert.deepEqual(computeModelChipState(download, status), { kind: "installed", sizeBytes: 500_000_000 });
});

test("chip state machine: downloading -> error -> back to offering Download", () => {
  let download = startDownload(0, 1000);
  download = applyProgressEvent(download, { downloaded_bytes: 100, total_bytes: 500_000_000 }, 1500);
  assert.equal(computeModelChipState(download, null).kind, "downloading");

  // transcription://model-download-error settles it the same way -done
  // does (handleModelDownloadSettled is shared between both events):
  // download clears to null, then a real transcription_model_status
  // fetch runs and (since the download never finished) reports absent.
  download = null;
  const status = { present: false, sizeBytes: null };
  assert.deepEqual(computeModelChipState(download, status), { kind: "offer-download" });
});

test("chip state machine: unknown before the first status fetch, check-failed if the fetch itself errors", () => {
  assert.deepEqual(computeModelChipState(null, null), { kind: "unknown" });
  assert.deepEqual(computeModelChipState(null, { error: true }), { kind: "check-failed" });
});

test("chip state machine: an active download always wins over status, whatever status says", () => {
  const download = startDownload(0, 1000);
  assert.equal(computeModelChipState(download, { present: true, sizeBytes: 1 }).kind, "downloading");
  assert.equal(computeModelChipState(download, { error: true }).kind, "downloading");
});

// ---------------------------------------------------------------------
// applyProgressEvent - the RESILIENCE #4 out-of-order/ghost-bar guard.
// ---------------------------------------------------------------------

test("applyProgressEvent: a straggler event with no active download is ignored (same reference back)", () => {
  const result = applyProgressEvent(null, { downloaded_bytes: 999, total_bytes: 1000 }, 5000);
  assert.equal(result, null);
});

test("applyProgressEvent: callers can detect 'ignored' via reference equality to the input", () => {
  // Contract this function's own doc promises: returning the SAME
  // reference (not just an equal-shaped null) is how app.js's
  // handleModelDownloadProgress knows not to re-render.
  const download = null;
  const result = applyProgressEvent(download, { downloaded_bytes: 1, total_bytes: 2 }, 1);
  assert.equal(result, download);
});

test("applyProgressEvent: updates bytes and lastProgressAt when a download is active", () => {
  const download = startDownload(0, 1000);
  const next = applyProgressEvent(download, { downloaded_bytes: 42, total_bytes: 84 }, 2000);
  assert.equal(next.downloadedBytes, 42);
  assert.equal(next.totalBytes, 84);
  assert.equal(next.lastProgressAt, 2000);
  assert.equal(next.generation, download.generation); // generation is untouched by progress
});

test("applyProgressEvent: clamps a negative downloaded_bytes to 0, never a negative-width bar", () => {
  const download = startDownload(0, 1000);
  const next = applyProgressEvent(download, { downloaded_bytes: -5, total_bytes: 100 }, 1500);
  assert.equal(next.downloadedBytes, 0);
  assert.equal(computeDownloadPct(next), 0);
});

test("applyProgressEvent: a missing total_bytes leaves the percentage indeterminate, not a divide-by-zero", () => {
  const download = startDownload(0, 1000);
  const next = applyProgressEvent(download, { downloaded_bytes: 500 }, 1200);
  assert.equal(next.totalBytes, null);
  assert.equal(computeDownloadPct(next), null);
});

// ---------------------------------------------------------------------
// generation counter - startDownload/nextGeneration.
// ---------------------------------------------------------------------

test("nextGeneration: increments from 0 and from any prior value, treats undefined/null as 0", () => {
  assert.equal(nextGeneration(0), 1);
  assert.equal(nextGeneration(1), 2);
  assert.equal(nextGeneration(undefined), 1);
  assert.equal(nextGeneration(null), 1);
});

test("startDownload: a fresh attempt always has a higher generation than the one before it", () => {
  const first = startDownload(0, 1000);
  const second = startDownload(first.generation, 5000);
  assert.ok(second.generation > first.generation);
  assert.equal(second.downloadedBytes, 0);
  assert.equal(second.totalBytes, null);
  assert.equal(second.lastProgressAt, 5000);
});

// ---------------------------------------------------------------------
// isDownloadStalled - the RESILIENCE #3 watchdog decision function (a
// sidecar that dies mid-download without a terminal event must not leave
// the chip reading "Downloading…" forever).
// ---------------------------------------------------------------------

test("isDownloadStalled: false with no active download", () => {
  assert.equal(isDownloadStalled(null, 100_000, 60_000), false);
});

test("isDownloadStalled: false while still within the timeout window", () => {
  const download = startDownload(0, 0);
  assert.equal(isDownloadStalled(download, 59_999, 60_000), false);
});

test("isDownloadStalled: true once the timeout has elapsed since the last progress", () => {
  const download = startDownload(0, 0);
  assert.equal(isDownloadStalled(download, 60_000, 60_000), true);
  assert.equal(isDownloadStalled(download, 120_000, 60_000), true);
});

test("isDownloadStalled: the clock resets on every real progress event", () => {
  let download = startDownload(0, 0);
  download = applyProgressEvent(download, { downloaded_bytes: 1, total_bytes: 100 }, 50_000);
  // 59_999ms after the LAST progress event (50_000), not the original
  // start (0) - would already have been "stalled" relative to start.
  assert.equal(isDownloadStalled(download, 109_999, 60_000), false);
  assert.equal(isDownloadStalled(download, 110_000, 60_000), true);
});

// ---------------------------------------------------------------------
// formatModelSize
// ---------------------------------------------------------------------

test("formatModelSize: MB below 1 GB, GB at and above", () => {
  assert.equal(formatModelSize(547_000_000), "547 MB");
  assert.equal(formatModelSize(999_999_999), "1000 MB");
  assert.equal(formatModelSize(1_000_000_000), "1.0 GB");
  assert.equal(formatModelSize(1_500_000_000), "1.5 GB");
});

test("formatModelSize: non-numeric/NaN input is an empty string, not 'NaN MB' or a throw", () => {
  assert.equal(formatModelSize(null), "");
  assert.equal(formatModelSize(undefined), "");
  assert.equal(formatModelSize(NaN), "");
  assert.equal(formatModelSize("547"), "");
});

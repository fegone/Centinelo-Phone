// Pure logic for Settings → Transcription (Plate 08) - extracted out of
// app.js (2026-07-17 4R re-review, RELIABILITY #1) specifically so it's
// testable without a DOM/window.__TAURI__ (app.js touches `window` at
// module scope and can't be imported under plain `node:test` the way this
// file can - same reasoning transcript-panel.js already applies to
// tapeHtml/fmtDuration/etc). Nothing here touches `document`, `invoke`, or
// any mutable module-level singleton - every function takes its inputs
// explicitly and returns a new value, so app.js's DOM/Tauri glue is the
// only thing left untested (by design: it's a thin, hard-to-get-wrong
// wrapper around what's tested here).

/// `transcription_model_status`/`download_transcription_model` speak the
/// `ModelTier` enum's own serde name ("accurate"/"light"), but the
/// `transcription://model-download-*` EVENTS instead carry
/// `tier_cli_name(tier)` - the real whisper.cpp model filename tier
/// (`transcription.rs` relays `centinelo-transcribe`'s own stdout protocol
/// lines verbatim, which speak in CLI tier names, not the shell's settings
/// enum). Pinned against `transcription.rs`'s own
/// `tier_cli_names_match_the_real_binarys_parse_function` test values -
/// see this file's own test suite's comment for the same pin from the JS
/// side.
export const MODEL_CLI_TIER_TO_SETTINGS_TIER = {
  "large-v3-turbo-q5_0": "accurate",
  "small-q5_1": "light",
};

export function mapCliTierToSettingsTier(cliTier) {
  return MODEL_CLI_TIER_TO_SETTINGS_TIER[cliTier] || null;
}

/// Builds the exact payload `save_transcription_settings` expects
/// (`commands::SaveTranscriptionInput` - mode/activation/keep_audio/
/// storage_dir/view_only/model_tier/language, snake_case) from this pane's
/// own camelCase-ish local state. Pulled out of saveAccountSettings so the
/// payload shape has a name and a test, instead of being an inline object
/// literal only ever exercised by hand-clicking through the app.
export function buildSaveTranscriptionInput({ mode, activation, keepAudio, storageDir, viewOnly, modelTier, language }) {
  return {
    mode,
    activation,
    keep_audio: !!keepAudio,
    storage_dir: (storageDir || "").trim(),
    view_only: !!viewOnly,
    model_tier: modelTier,
    language,
  };
}

/// `1.0 GB`/`547 MB` - the plate's own mono chip register (TOKENS.md
/// "Numbers are facts"). GB only once it's actually ≥1000 MB; every model
/// this app ships today (large-v3-turbo-q5_0, small-q5_1) is well under
/// 2 GB, so one decimal place of GB is enough precision to stay honest
/// without turning into "1.04857 GB".
export function formatModelSize(bytes) {
  if (typeof bytes !== "number" || !Number.isFinite(bytes)) return "";
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${Math.round(bytes / 1e6)} MB`;
}

// ---------------------------------------------------------------------------
// model download tracking - a `download` record is
// `{ generation, downloadedBytes, totalBytes, lastProgressAt } | null`.
// `generation` is a purely client-side monotonic counter (the backend's
// events carry no request id to correlate against - see
// `applyProgressEvent`'s doc for exactly what that does and doesn't
// protect against); `lastProgressAt` is an epoch-ms timestamp used by the
// watchdog (RESILIENCE #3) to detect a download that's gone silent
// because its sidecar died without ever emitting a terminal event.
// ---------------------------------------------------------------------------

export function nextGeneration(prevGeneration) {
  return (prevGeneration || 0) + 1;
}

/// Starts a fresh download record. `prevGeneration` is whatever this tier's
/// generation counter last was (0 if never downloaded before this session) -
/// the caller (app.js) owns that counter's storage since it must survive
/// across a download settling back to `null`, which is why it isn't read
/// off a previous `download` record here.
export function startDownload(prevGeneration, now) {
  return { generation: nextGeneration(prevGeneration), downloadedBytes: 0, totalBytes: null, lastProgressAt: now };
}

/// Applies a `transcription://model-download-progress` payload to
/// `download`. Returns the SAME `download` reference, unchanged, when
/// there's no active download to apply it to (`download` is `null`) -
/// callers should treat "returned the same reference" as "ignore this
/// event, nothing to re-render" (see app.js's handleModelDownloadProgress).
///
/// This is the RESILIENCE #4 guard: a straggler progress event that
/// arrives AFTER `transcription://model-download-done`/`-error` already
/// cleared this tier's record back to `null` must not revive a ghost
/// progress bar. It does NOT, and structurally cannot, distinguish a
/// straggler from an *older* download's generation while a *newer*
/// download for the same tier is already active - the backend's events
/// carry no generation/request id to check against, only a tier name -
/// but `startModelDownload` already refuses to start a second download
/// while one is tracked as active (`if (modelDownload[tier]) return;`),
/// so the only way to reach "download active, event belongs to a
/// different attempt" is the done/error boundary this function does
/// guard.
///
/// `downloaded_bytes` is clamped to a floor of 0 - a negative value would
/// be a wire/parsing bug upstream, and this pane would rather show 0%
/// than a nonsensical negative-width progress bar.
export function applyProgressEvent(download, payload, now) {
  if (!download) return download;
  const downloadedBytes = Math.max(0, (payload && payload.downloaded_bytes) || 0);
  const totalBytes = (payload && payload.total_bytes) || null;
  return { ...download, downloadedBytes, totalBytes, lastProgressAt: now };
}

/// True once `download` has gone `timeoutMs` without a progress event
/// (or, if none has ever landed, since it started) - the watchdog's decision
/// function (RESILIENCE #3: a sidecar that dies mid-download without
/// emitting `-done`/`-error` would otherwise leave the chip reading
/// "Downloading…" forever).
export function isDownloadStalled(download, now, timeoutMs) {
  if (!download) return false;
  return now - download.lastProgressAt >= timeoutMs;
}

/// 0-100, clamped both ends, or `null` while no `total_bytes` has arrived
/// yet (the chip then shows an indeterminate "Downloading…" label instead
/// of a bogus 0%/100%).
export function computeDownloadPct(download) {
  if (!download || !download.totalBytes) return null;
  return Math.max(0, Math.min(100, Math.round((download.downloadedBytes / download.totalBytes) * 100)));
}

// ---------------------------------------------------------------------------
// model status chip state machine
//
// `status` (this tier's last known `transcription_model_status` result) is
// one of: `null` (not fetched yet this Settings session), `{error: true}`
// (the fetch itself failed - RESILIENCE #5: must NOT be conflated with
// "confirmed absent", which would offer a live "Download" button for a
// model that might already be installed and just failed a status check),
// or `{present, sizeBytes}` (a real answer from disk).
// ---------------------------------------------------------------------------

/// Pure state -> chip-state-to-render mapping - the "state machine" this
/// pane's model rows go through: `unknown` (nothing fetched yet) ->
/// `offer-download`/`installed`/`check-failed` (transcription_model_status
/// resolved) -> `downloading` (a download is active, always wins over
/// `status` regardless of what `status` says - the chip is about "what's
/// happening right now") -> back to `offer-download`/`installed` once it
/// settles and `refreshModelStatuses` re-fetches the real disk state (never
/// inferred from which event fired - see app.js's
/// handleModelDownloadSettled doc).
export function computeModelChipState(download, status) {
  if (download) {
    return { kind: "downloading", pct: computeDownloadPct(download) };
  }
  if (!status) {
    return { kind: "unknown" };
  }
  if (status.error) {
    return { kind: "check-failed" };
  }
  if (status.present) {
    return { kind: "installed", sizeBytes: status.sizeBytes ?? null };
  }
  return { kind: "offer-download" };
}

#!/usr/bin/env bash
# Mock `centinelo-transcribe` binary for shell-side tests (src/transcription.rs).
#
# Speaks the REAL contract confirmed against transcribe-engine's actual
# implementation (premium repo, `feature/transcribe-e2e`, read read-only
# for this shell's own integration - not reproduced/edited here beyond
# this mock): `"type"` as the JSON discriminator key (not `"event"`),
# `done`'s fields are `txt`/`json`/`channels_failed` (not `txt_path`/
# `json_path`, and not missing the partial-transcript flag added in a
# 2026-07-16 reliability re-review on transcribe-engine's side).
#
# CENTINELO_MOCK_CHANNELS_FAILED (optional, run mode only): a comma-
# separated list of speakers (e.g. "agent" or "agent,caller") to report in
# `done`'s `channels_failed` array - lets a test exercise the shell's
# partial-transcript handling without needing a real corrupt WAV. Empty/
# unset -> `[]`, the default clean-run shape.
#
# Subcommands:
#   run --rx ... --tx ... --model ... --lang ... --mode live|post
#       --out-dir ... --meta ...
#     Emits two synthetic `segment` events, then a `done` event pointing
#     at real files it actually writes (so finalize_artifacts's move/
#     rename logic can be exercised end-to-end against real output). In
#     `--mode live`, blocks after the segments until it reads a `stop`
#     line on stdin, per the live-mode contract.
#   ensure-model --tier ... --models-dir ...
#     Emits one `progress` event then a `ready` event - no real download,
#     just proves the shell's spawn/parse code against the real CLI shape.
#
# No real audio is read - the `--rx`/`--tx` WAV paths are accepted for
# CLI-shape fidelity but never opened, since this script fabricates its
# transcript rather than actually transcribing. Synthetic text only, no
# PHI, no real call audio, per this repo's testing rules.
set -euo pipefail

SUBCOMMAND="${1:-}"
shift || true

if [[ "$SUBCOMMAND" == "ensure-model" ]]; then
  TIER=""
  MODELS_DIR="."
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --tier) TIER="$2"; shift 2 ;;
      --models-dir) MODELS_DIR="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  mkdir -p "$MODELS_DIR"
  echo "{\"type\":\"progress\",\"asset\":\"ggml-${TIER}.bin\",\"downloaded\":1024,\"total\":2048}"
  echo "{\"type\":\"ready\",\"model\":\"${MODELS_DIR}/ggml-${TIER}.bin\",\"vad_model\":\"${MODELS_DIR}/ggml-silero-v5.1.2.bin\"}"
  exit 0
fi

if [[ "$SUBCOMMAND" != "run" ]]; then
  echo "{\"type\":\"error\",\"message\":\"mock-transcribe.sh: unknown subcommand '$SUBCOMMAND'\"}"
  exit 1
fi

MODE=""
OUT_DIR="."

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rx) shift 2 ;;
    --tx) shift 2 ;;
    --model) shift 2 ;;
    --lang) shift 2 ;;
    --mode) MODE="$2"; shift 2 ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    --meta) shift 2 ;;
    *) shift ;;
  esac
done

mkdir -p "$OUT_DIR"
TXT="$OUT_DIR/transcript.txt"
JSON="$OUT_DIR/transcript.json"

echo '{"type":"segment","speaker":"agent","t0_ms":0,"t1_ms":900,"text":"hello, thanks for calling"}'
echo '{"type":"segment","speaker":"caller","t0_ms":1000,"t1_ms":2200,"text":"hola, tengo una pregunta"}'

if [[ "$MODE" == "live" ]]; then
  # Live mode: wait for the "stop" line on stdin before wrapping up -
  # see transcription.rs's on_tap_stopped, which writes exactly that.
  while IFS= read -r line; do
    if [[ "$line" == "stop" ]]; then
      break
    fi
  done
fi

printf '[00:00] agent: hello, thanks for calling\n[00:01] caller: hola, tengo una pregunta\n' > "$TXT"
printf '{"segments":[]}\n' > "$JSON"

CHANNELS_FAILED_JSON="[]"
if [[ -n "${CENTINELO_MOCK_CHANNELS_FAILED:-}" ]]; then
  CHANNELS_FAILED_JSON=$(printf '%s' "$CENTINELO_MOCK_CHANNELS_FAILED" | awk -F',' '{
    printf "["
    for (i = 1; i <= NF; i++) { printf "%s\"%s\"", (i > 1 ? "," : ""), $i }
    printf "]"
  }')
fi

echo "{\"type\":\"done\",\"txt\":\"$TXT\",\"json\":\"$JSON\",\"channels_failed\":$CHANNELS_FAILED_JSON}"

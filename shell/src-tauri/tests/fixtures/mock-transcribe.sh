#!/usr/bin/env bash
# Mock `centinelo-transcribe` binary for shell-side tests (src/transcription.rs).
#
# Speaks the JSON-lines contract this shell integrates against (see
# transcription.rs's module doc for the full CLI/event shape): reads the
# same flags the real sidecar will take, emits two synthetic `segment`
# events, and finishes with a `done` event pointing at real files it
# actually writes (so finalize_artifacts's move/rename logic can be
# exercised end-to-end against real output, not just parsed JSON). In
# `--mode live`, it blocks after the segments until it reads a `stop`
# line on stdin, per the live-mode contract.
#
# No real audio is read - the `--rx`/`--tx` WAV paths are accepted for
# CLI-shape fidelity but never opened, since this script fabricates its
# transcript rather than actually transcribing. Synthetic text only, no
# PHI, no real call audio, per this repo's testing rules.
set -euo pipefail

MODE=""
OUT_DIR="."

while [[ $# -gt 0 ]]; do
  case "$1" in
    run) shift ;;
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

echo '{"event":"segment","speaker":"agent","t0_ms":0,"t1_ms":900,"text":"hello, thanks for calling"}'
echo '{"event":"segment","speaker":"caller","t0_ms":1000,"t1_ms":2200,"text":"hola, tengo una pregunta"}'

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

echo "{\"event\":\"done\",\"txt_path\":\"$TXT\",\"json_path\":\"$JSON\"}"

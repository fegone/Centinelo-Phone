#!/usr/bin/env bash
#
# core/run-spike.sh - launch the Centinelo Phone v2 baresip engine spike.
#
# Generates a throwaway baresip config (config + accounts) in a scratch
# directory from environment variables, then execs baresip with a minimal
# module set (see BUILD.md) including our out-of-tree ctrl_json control
# module. The SIP secret is never hardcoded here or written anywhere
# outside the scratch dir's `accounts` file (mode 0600, gitignored,
# deleted with the rest of the scratch dir).
#
# Required env vars:
#   CENT_EXT      SIP extension / username, e.g. 1100
#   CENT_SECRET   SIP auth password for that extension
#   CENT_HOST     PBX host/IP, e.g. 100.119.230.80
#
# Optional env vars:
#   CENT_TRANSPORT      wss (default) | udp | tcp | tls
#   CENT_PORT           overrides the default port for CENT_TRANSPORT
#                        (wss->8089, tls->5061, tcp/udp->5060)
#   CENT_SCRATCH_DIR     reuse a specific scratch dir instead of a fresh
#                        mktemp one (it will be created if missing)
#   CENT_VERIFY_SERVER   yes|no (default: no). The PBX WSS endpoint used
#                        for this spike serves a self-signed/internal-CA
#                        cert - see BUILD.md "TLS leaf-certificate
#                        pinning" for CENT_TLS_PIN, below, which adds an
#                        independent check on top of this.
#   CENT_WS_PATH         HTTP path for the ws/wss upgrade request
#                        (default: /ws, matching Asterisk's default
#                        `res_http_websocket` mount point). Consumed by
#                        the re patch in core/patches/ - see BUILD.md
#                        "Findings" for why this exists: stock re/baresip
#                        hardcode "/", which 404s against Asterisk.
#   CENT_TLS_PIN         sha256 hex (colons/spaces tolerated) of the
#                        expected WSS server leaf cert, DER bytes -
#                        matching the v1 Electron app's
#                        settings.pinnedCertSha256 entries. Optional;
#                        unset = pre-F1 behavior (no pin check, only
#                        whatever CENT_VERIFY_SERVER configures). Passed
#                        straight through to the child baresip process's
#                        environment (no default computed here, unlike
#                        CENT_WS_PATH) - consumed by the re patch in
#                        core/patches/0002-*, see BUILD.md "TLS
#                        leaf-certificate pinning".
#   CENT_BARESIP_ARGS    extra args appended to the baresip invocation
#                        unquoted/word-split on purpose, e.g.
#                        CENT_BARESIP_ARGS="-t 30" to auto-quit after 30s
#                        (-s adds SIP trace - see PROTOCOL.md "Framing"
#                        for where that output actually lands)
#
# I/O once running:
#   stdin  - newline-delimited JSON commands (see PROTOCOL.md), on
#            Windows read by a dedicated thread (see PROTOCOL.md
#            "Framing / stdin"), otherwise identical either platform
#   stdout - newline-delimited JSON events (see PROTOCOL.md), interleaved
#            with non-JSON noise (baresip's startup banner, some
#            module-load log lines, and - only if CENT_BARESIP_ARGS
#            includes -s - raw SIP trace) - see PROTOCOL.md "Framing" for
#            the full why, and filter for lines starting with '{' if you
#            need a strictly-JSON stream.
#   stderr - baresip's own human-readable debug/info/warning log.
#
# Example:
#   CENT_EXT=1100 CENT_HOST=100.119.230.80 CENT_TRANSPORT=wss \
#     CENT_SECRET="$(...)" ./core/run-spike.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/deps/baresip/build"
BARESIP_BIN="$BUILD_DIR/baresip"

: "${CENT_EXT:?CENT_EXT (SIP extension) is required}"
: "${CENT_SECRET:?CENT_SECRET (SIP auth password) is required}"
: "${CENT_HOST:?CENT_HOST (PBX host/IP) is required}"

CENT_TRANSPORT="${CENT_TRANSPORT:-wss}"
CENT_VERIFY_SERVER="${CENT_VERIFY_SERVER:-no}"
export CENT_WS_PATH="${CENT_WS_PATH:-/ws}"

if [ ! -x "$BARESIP_BIN" ]; then
	echo "error: $BARESIP_BIN not found - build it first, see core/BUILD.md" >&2
	exit 1
fi

case "$CENT_TRANSPORT" in
	wss) default_port=8089 ;;
	tls) default_port=5061 ;;
	tcp|udp) default_port=5060 ;;
	*)
		echo "error: unsupported CENT_TRANSPORT '$CENT_TRANSPORT' (want wss|udp|tcp|tls)" >&2
		exit 1
		;;
esac
CENT_PORT="${CENT_PORT:-$default_port}"

SCRATCH_DIR="${CENT_SCRATCH_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/centinelo-spike.XXXXXX")}"
mkdir -p "$SCRATCH_DIR"

# --- accounts file ---------------------------------------------------------
#
# The PBX endpoint for the test extension is provisioned with webrtc=yes
# (confirmed read-only via `asterisk -rx "pjsip show endpoint <ext>"`),
# which forces media_encryption=dtls + ice_support=yes at the endpoint
# level regardless of which SIP *signaling* transport is used to reach it.
# So mediaenc=dtls_srtp / medianat=ice / rtcp_mux=yes are required here
# unconditionally, not just for the wss case - see PROTOCOL.md and
# BUILD.md findings for the UDP registration attempt.
#
# `outbound` pins an explicit proxy URI (same host/port/transport as the
# registration) so a bare `dial sip:ext@host` - with no ;transport= or
# :port of its own, exactly what PROTOCOL.md's v0 dial command sends -
# still routes over the transport under test instead of re::sip resolving
# the request-URI fresh and defaulting to the wss/tls well-known port
# (443), which nothing is listening on here. Found by running the spike:
# without `outbound`, dialing *43 over wss silently tried to connect to
# 100.119.230.80:443 and failed. See BUILD.md "Findings".
umask 077   # accounts contains auth_pass - keep the scratch dir private
ACCOUNT_URI="<sip:${CENT_EXT}@${CENT_HOST}:${CENT_PORT};transport=${CENT_TRANSPORT}>"
ACCOUNT_PARAMS=";auth_pass=${CENT_SECRET};mediaenc=dtls_srtp;medianat=ice;rtcp_mux=yes;audio_codecs=pcmu,pcma;regint=120"
ACCOUNT_PARAMS="${ACCOUNT_PARAMS};outbound=\"sip:${CENT_HOST}:${CENT_PORT};transport=${CENT_TRANSPORT}\""
printf '%s%s\n' "$ACCOUNT_URI" "$ACCOUNT_PARAMS" > "$SCRATCH_DIR/accounts"

# --- config file ------------------------------------------------------------
#
# Minimal, explicit module list (see BUILD.md "Module selection") instead
# of baresip's own everything-enabled default MODULES cache list: g711
# (matches the endpoint's allow=(opus|ulaw)) + auconv/auresamp (format
# glue) + ausine (sine-wave ausrc, no microphone/OS audio permission
# needed) + aufile (writes received audio to a wav file as auplay, no
# speaker needed) + ice + dtls_srtp (required by webrtc=yes, see above) +
# menu (owns the dial/accept/hangup long commands ctrl_json drives via
# cmd_process_long) + our ctrl_json app module. `account` (which loads
# the accounts file below) is listed LAST on purpose: it validates the
# account's audio_codecs=/medianat=/mediaenc= restrictions against
# already-registered codecs/mnat/menc at parse time, so it must load
# *after* g711/ice/dtls_srtp or those restrictions silently fail to bind
# (found by running the spike: "account: medianat not found: 'ice'" etc.
# when account.so was first in the list).
cat > "$SCRATCH_DIR/config" <<EOF
# Generated by run-spike.sh - do not edit by hand, do not commit.

module_path		${BUILD_DIR}

module			g711.so
module			auconv.so
module			auresamp.so
module			ausine.so
module			aufile.so
module			ice.so
module			dtls_srtp.so
module			menu.so
module			account.so
module_app		ctrl_json.so

# Self-signed/internal-CA WSS cert for this spike - see BUILD.md
# "TODO: cert pinning" before this ever leaves the spike stage.
sip_verify_server	${CENT_VERIFY_SERVER}

audio_source		ausine,440
audio_player		aufile,${SCRATCH_DIR}/rx.wav
audio_alert		aufile,${SCRATCH_DIR}/rx.wav

rtp_timeout		0
EOF

{
	echo "centinelo: scratch dir     = $SCRATCH_DIR"
	echo "centinelo: transport       = $CENT_TRANSPORT (port $CENT_PORT)"
	echo "centinelo: pbx host        = $CENT_HOST"
	echo "centinelo: extension       = $CENT_EXT"
	echo "centinelo: sip_verify_server = $CENT_VERIFY_SERVER"
} >&2

# shellcheck disable=SC2086
exec "$BARESIP_BIN" -f "$SCRATCH_DIR" ${CENT_BARESIP_ARGS:-}

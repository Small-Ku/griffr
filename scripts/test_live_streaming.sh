#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 5 ]]; then
    cat >&2 <<'USAGE'
Usage: scripts/test_live_streaming.sh <root> <game> <region> [channel] [sub-channel]

The root must be a dedicated directory. Every official package part is streamed
through Griffr's bounded HTTP writer, size/MD5 verified, atomically committed,
and immediately removed. This is a payload-integrity soak, not a retained
install, repair, or hardlink lifecycle test.
USAGE
    exit 2
fi

unset GRIFFR_LIVE_E2E_CHANNEL GRIFFR_LIVE_E2E_SUB_CHANNEL
export GRIFFR_LIVE_E2E_ROOT=$1
export GRIFFR_LIVE_E2E_GAME=$2
export GRIFFR_LIVE_E2E_REGION=$3
export GRIFFR_LIVE_E2E_CONFIRM=I_ACCEPT_LARGE_DOWNLOADS_AND_TEST_DELETION
[[ $# -ge 4 && -n $4 ]] && export GRIFFR_LIVE_E2E_CHANNEL=$4
[[ $# -ge 5 && -n $5 ]] && export GRIFFR_LIVE_E2E_SUB_CHANNEL=$5

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
cargo test -p griffr-cli --test live_cli_e2e --locked official_server_streaming_package_soak -- --ignored --nocapture

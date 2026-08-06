#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 8 ]]; then
    cat >&2 <<'USAGE'
Usage: scripts/test_live_cli_e2e.sh <root> <game> <region> [channel] [sub-channel] [resources] [disposable-update-path] [fetch-predownload]

resources defaults to base and may be set to off, base, or all.
fetch-predownload may be 1, true, or yes.
The root must be a dedicated directory on the filesystem whose hardlink behavior
is being tested. The test downloads a full current install and deletes only its
own run-* child directory after success. A disposable update path is updated and
verified but never deleted by the harness.
USAGE
    exit 2
fi

unset GRIFFR_LIVE_E2E_CHANNEL GRIFFR_LIVE_E2E_SUB_CHANNEL     GRIFFR_LIVE_E2E_UPDATE_PATH GRIFFR_LIVE_E2E_FETCH_PREDOWNLOAD
export GRIFFR_LIVE_E2E_ROOT=$1
export GRIFFR_LIVE_E2E_GAME=$2
export GRIFFR_LIVE_E2E_REGION=$3
export GRIFFR_LIVE_E2E_CONFIRM=I_ACCEPT_LARGE_DOWNLOADS_AND_TEST_DELETION
[[ $# -ge 4 && -n $4 ]] && export GRIFFR_LIVE_E2E_CHANNEL=$4
[[ $# -ge 5 && -n $5 ]] && export GRIFFR_LIVE_E2E_SUB_CHANNEL=$5
export GRIFFR_LIVE_E2E_RESOURCES=${6:-base}
[[ $# -ge 7 && -n $7 ]] && export GRIFFR_LIVE_E2E_UPDATE_PATH=$7
[[ $# -ge 8 && -n $8 ]] && export GRIFFR_LIVE_E2E_FETCH_PREDOWNLOAD=$8

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
cargo test -p griffr-cli --test live_cli_e2e --locked -- --ignored --nocapture

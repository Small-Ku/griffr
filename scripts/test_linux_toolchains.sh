#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/test_linux_toolchains.sh <stable-toolchain-root> <nightly-toolchain-root>

Runs the repository policy checks, the complete stable Rust validation, and the
real-process CLI E2E test again with nightly rustc_codegen_cranelift. Toolchain
roots must contain bin/cargo and bin/rustc, as produced by the official Linux
Rust tarballs after installation/extraction.
EOF
}

if [[ $# -ne 2 ]]; then
    usage >&2
    exit 2
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
stable_root=$(cd "$1" && pwd)
nightly_root=$(cd "$2" && pwd)

for executable in \
    "$stable_root/bin/cargo" \
    "$stable_root/bin/rustc" \
    "$nightly_root/bin/cargo" \
    "$nightly_root/bin/rustc"; do
    if [[ ! -x "$executable" ]]; then
        printf 'Missing executable toolchain component: %s\n' "$executable" >&2
        exit 2
    fi
done

cd "$repo_root"

python3 scripts/check_repo.py .
python3 -m unittest discover -s scripts/tests -v

stable_target=${GRIFFR_STABLE_TARGET_DIR:-target/toolchain-stable}
nightly_target=${GRIFFR_NIGHTLY_TARGET_DIR:-target/toolchain-nightly-cranelift}

printf '\n==> Stable toolchain: %s\n' "$("$stable_root/bin/rustc" --version)"
PATH="$stable_root/bin:$PATH" \
RUSTC="$stable_root/bin/rustc" \
CARGO_TARGET_DIR="$stable_target" \
    "$stable_root/bin/cargo" fmt --all -- --check
PATH="$stable_root/bin:$PATH" \
RUSTC="$stable_root/bin/rustc" \
CARGO_TARGET_DIR="$stable_target" \
    "$stable_root/bin/cargo" check --workspace --all-targets --locked --offline
PATH="$stable_root/bin:$PATH" \
RUSTC="$stable_root/bin/rustc" \
CARGO_TARGET_DIR="$stable_target" \
    "$stable_root/bin/cargo" clippy --workspace --all-targets --locked --offline -- -D warnings
PATH="$stable_root/bin:$PATH" \
RUSTC="$stable_root/bin/rustc" \
CARGO_TARGET_DIR="$stable_target" \
    "$stable_root/bin/cargo" test --workspace --locked --offline

printf '\n==> Nightly Cranelift toolchain: %s\n' "$("$nightly_root/bin/rustc" --version)"
PATH="$nightly_root/bin:$PATH" \
RUSTC="$nightly_root/bin/rustc" \
CARGO_TARGET_DIR="$nightly_target" \
    "$nightly_root/bin/cargo" -Zcodegen-backend \
    --config 'profile.dev.codegen-backend="cranelift"' \
    check --workspace --all-targets --locked --offline
PATH="$nightly_root/bin:$PATH" \
RUSTC="$nightly_root/bin/rustc" \
CARGO_TARGET_DIR="$nightly_target" \
    "$nightly_root/bin/cargo" -Zcodegen-backend \
    --config 'profile.test.codegen-backend="cranelift"' \
    test -p griffr-cli --test cli_e2e --locked --offline -- --nocapture

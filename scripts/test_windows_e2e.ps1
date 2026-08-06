[CmdletBinding()]
param(
    [string]$Toolchain = "1.97.1",
    [string]$TargetDir = "target/platform-windows"
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

rustup toolchain install $Toolchain --profile minimal --component rustfmt,clippy
$env:RUSTUP_TOOLCHAIN = $Toolchain

python scripts/check_repo.py .
python -m unittest discover -s scripts/tests -v

$env:CARGO_TARGET_DIR = $TargetDir
cargo fmt --all -- --check
cargo check -p griffr-cli --tests --locked
cargo clippy -p griffr-cli --tests --locked -- -D warnings
cargo test -p griffr-cli --test cli_e2e --locked -- --nocapture

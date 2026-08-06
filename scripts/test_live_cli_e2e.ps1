[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Root,
    [Parameter(Mandatory = $true)][string]$Game,
    [Parameter(Mandatory = $true)][string]$Region,
    [string]$Channel,
    [string]$SubChannel,
    [ValidateSet("off", "base", "all")][string]$Resources = "base",
    [string]$DisposableUpdatePath,
    [switch]$FetchPredownload
)

$ErrorActionPreference = "Stop"
Remove-Item Env:GRIFFR_LIVE_E2E_CHANNEL -ErrorAction SilentlyContinue
Remove-Item Env:GRIFFR_LIVE_E2E_SUB_CHANNEL -ErrorAction SilentlyContinue
Remove-Item Env:GRIFFR_LIVE_E2E_UPDATE_PATH -ErrorAction SilentlyContinue
Remove-Item Env:GRIFFR_LIVE_E2E_FETCH_PREDOWNLOAD -ErrorAction SilentlyContinue
$env:GRIFFR_LIVE_E2E_ROOT = $Root
$env:GRIFFR_LIVE_E2E_GAME = $Game
$env:GRIFFR_LIVE_E2E_REGION = $Region
$env:GRIFFR_LIVE_E2E_RESOURCES = $Resources
$env:GRIFFR_LIVE_E2E_CONFIRM = "I_ACCEPT_LARGE_DOWNLOADS_AND_TEST_DELETION"
if ($Channel) { $env:GRIFFR_LIVE_E2E_CHANNEL = $Channel }
if ($SubChannel) { $env:GRIFFR_LIVE_E2E_SUB_CHANNEL = $SubChannel }
if ($DisposableUpdatePath) { $env:GRIFFR_LIVE_E2E_UPDATE_PATH = $DisposableUpdatePath }
if ($FetchPredownload) { $env:GRIFFR_LIVE_E2E_FETCH_PREDOWNLOAD = "1" }

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot
cargo test -p griffr-cli --test live_cli_e2e --locked -- --ignored --nocapture

# Quick live predownload check for Endfield launcher API.
# Run from repo root, for example:
#   .\scripts\research\check_predownload.ps1
#   .\scripts\research\check_predownload.ps1 -Server cn-bilibili -Version 1.2.5 -ShowUrls

[CmdletBinding()]
param(
    [ValidateSet("cn-official", "cn-bilibili", "os-official", "os-epic", "os-googleplay")]
    [string]$Server = "cn-official",

    [string]$Version = "1.2.5",

    [switch]$ShowUrls,

    [string]$OutputRaw
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-ServerConfig {
    param([string]$ServerName)

    switch ($ServerName) {
        "cn-official" {
            return @{
                region = "cn"
                endpoint = "https://launcher.hypergryph.com/api/proxy/batch_proxy"
                appcode = "6LL0KJuqHBVz33WK"
                launcher_appcode = "abYeZZ16BPluCFyT"
                channel = "1"
                sub_channel = "1"
            }
        }
        "cn-bilibili" {
            return @{
                region = "cn"
                endpoint = "https://launcher.hypergryph.com/api/proxy/batch_proxy"
                appcode = "6LL0KJuqHBVz33WK"
                launcher_appcode = "abYeZZ16BPluCFyT"
                channel = "2"
                sub_channel = "2"
            }
        }
        "os-official" {
            return @{
                region = "os"
                endpoint = "https://launcher.gryphline.com/api/proxy/batch_proxy"
                appcode = "YDUTE5gscDZ229CW"
                launcher_appcode = "TiaytKBUIEdoEwRT"
                channel = "6"
                sub_channel = "6"
            }
        }
        "os-epic" {
            return @{
                region = "os"
                endpoint = "https://launcher.gryphline.com/api/proxy/batch_proxy"
                appcode = "YDUTE5gscDZ229CW"
                launcher_appcode = "BBWoqCzuZ2bZ1Dro"
                channel = "6"
                sub_channel = "801"
            }
        }
        "os-googleplay" {
            return @{
                region = "os"
                endpoint = "https://launcher.gryphline.com/api/proxy/batch_proxy"
                appcode = "YDUTE5gscDZ229CW"
                launcher_appcode = "TiaytKBUIEdoEwRT"
                channel = "6"
                sub_channel = "802"
            }
        }
    }

    throw "Unsupported server: $ServerName"
}

function Format-Bytes {
    param([object]$Value)

    $bytes = 0L
    if ($null -ne $Value) {
        [void][long]::TryParse([string]$Value, [ref]$bytes)
    }

    if ($bytes -ge 1GB) {
        return ("{0:N2} GiB" -f ($bytes / 1GB))
    }
    if ($bytes -ge 1MB) {
        return ("{0:N2} MiB" -f ($bytes / 1MB))
    }
    if ($bytes -ge 1KB) {
        return ("{0:N2} KiB" -f ($bytes / 1KB))
    }
    return "$bytes B"
}

$config = Get-ServerConfig -ServerName $Server

$body = [ordered]@{
    seq = "1"
    proxy_reqs = @(
        [ordered]@{
            kind = "get_latest_game"
            get_latest_game_req = [ordered]@{
                appcode = $config.appcode
                channel = $config.channel
                sub_channel = $config.sub_channel
                version = $Version
                launcher_appcode = $config.launcher_appcode
            }
        }
    )
}

$jsonBody = $body | ConvertTo-Json -Depth 10 -Compress

Write-Host "Checking Endfield predownload..." -ForegroundColor Cyan
Write-Host "  Server:  $Server" -ForegroundColor Gray
Write-Host "  Version: $Version" -ForegroundColor Gray
Write-Host "  API:     $($config.endpoint)" -ForegroundColor Gray

$response = Invoke-RestMethod `
    -Uri $config.endpoint `
    -Method Post `
    -ContentType "application/json" `
    -Body $jsonBody `
    -TimeoutSec 30

if (-not $OutputRaw) {
    $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $tmpDir = Join-Path $repoRoot "tmp"
    New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $OutputRaw = Join-Path $tmpDir "predownload-check-$Server-$stamp.json"
}

$response | ConvertTo-Json -Depth 20 | Set-Content -Path $OutputRaw -Encoding UTF8
Write-Host "  Raw response saved to: $OutputRaw" -ForegroundColor Gray

$latest = $response.proxy_rsps | Where-Object { $_.kind -eq "get_latest_game" } | Select-Object -First 1
if (-not $latest) {
    Write-Error "Missing get_latest_game response."
}

$rsp = $latest.get_latest_game_rsp
if (-not $rsp) {
    Write-Error "Missing get_latest_game_rsp payload."
}

$hasPrePatch = $null -ne $rsp.pre_patch -and $rsp.pre_patch.patches.Count -gt 0

Write-Host ""
Write-Host "Response summary" -ForegroundColor Cyan
Write-Host "  request_version: $($rsp.request_version)" -ForegroundColor Gray
Write-Host "  version:         $($rsp.version)" -ForegroundColor Gray
Write-Host "  action:          $($rsp.action)" -ForegroundColor Gray
Write-Host "  launcher_action: $($rsp.launcher_action)" -ForegroundColor Gray
Write-Host "  has pkg:         $([bool]($rsp.pkg -and $rsp.pkg.packs.Count -gt 0))" -ForegroundColor Gray
Write-Host "  has patch:       $([bool]($rsp.patch -and $rsp.patch.patches.Count -gt 0))" -ForegroundColor Gray
Write-Host "  has pre_patch:   $hasPrePatch" -ForegroundColor Gray

if (-not $hasPrePatch) {
    Write-Host ""
    Write-Host "No predownload payload is present in this response." -ForegroundColor Yellow
    exit 3
}

$prePatch = $rsp.pre_patch
$parts = @($prePatch.patches)

Write-Host ""
Write-Host "Predownload available" -ForegroundColor Green
Write-Host "  target version:  $($prePatch.version)" -ForegroundColor Green
Write-Host "  parts:           $($parts.Count)" -ForegroundColor Gray
Write-Host "  packed size:     $(Format-Bytes $prePatch.package_size) ($($prePatch.package_size))" -ForegroundColor Gray
Write-Host "  total size:      $(Format-Bytes $prePatch.total_size) ($($prePatch.total_size))" -ForegroundColor Gray

$index = 0
foreach ($part in $parts) {
    $index += 1
    $url = [string]$part.url
    $fileName = ($url.Split("/")[-1] -split "\?")[0]
    Write-Host ("  [{0}] {1}  {2}" -f $index, $fileName, (Format-Bytes $part.package_size)) -ForegroundColor Gray
    if ($ShowUrls) {
        Write-Host "      $url" -ForegroundColor DarkGray
        Write-Host "      md5=$($part.md5)" -ForegroundColor DarkGray
    }
}

exit 0

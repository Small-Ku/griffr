# Compare Channel Files for Endfield (Research-only helper)
# Run from repo root: .\scripts\research\compare_channels.ps1
# Output files are written under .\tmp\

# AES-256-CBC Decryption for game_files manifest
function Decrypt-GameFiles {
    param([byte[]]$EncryptedBytes)

    $Key = [byte[]]@(0xC0, 0xF3, 0x0E, 0x1C, 0xE7, 0x63, 0xBB, 0xC2, 0x1C, 0xC3, 0x55, 0xA3, 0x43, 0x03, 0xAC, 0x50,
                     0x39, 0x94, 0x44, 0xBF, 0xF6, 0x8C, 0x4A, 0x22, 0xAF, 0x39, 0x8C, 0x0A, 0x16, 0x6E, 0xE1, 0x43)
    $IV = [byte[]]@(0x33, 0x46, 0x78, 0x61, 0x19, 0x27, 0x50, 0x64, 0x95, 0x01, 0x93, 0x72, 0x64, 0x60, 0x84, 0x00)

    try {
        $Aes = [System.Security.Cryptography.Aes]::Create()
        $Aes.Key = $Key
        $Aes.IV = $IV
        $Aes.Mode = [System.Security.Cryptography.CipherMode]::CBC
        $Aes.Padding = [System.Security.Cryptography.PaddingMode]::PKCS7

        $Decryptor = $Aes.CreateDecryptor()
        $DecryptedBytes = $Decryptor.TransformFinalBlock($EncryptedBytes, 0, $EncryptedBytes.Length)
        $Decryptor.Dispose()
        $Aes.Dispose()

        $DecryptedText = [System.Text.Encoding]::UTF8.GetString($DecryptedBytes)
        
        # Parse JSON Lines format (one JSON object per line)
        $Files = @()
        $Lines = $DecryptedText -split "`n" | Where-Object { $_.Trim() }
        foreach ($Line in $Lines) {
            try {
                $FileObj = $Line | ConvertFrom-Json -ErrorAction Stop
                $Files += $FileObj
            } catch {
                # Skip invalid lines
            }
        }
        return $Files
    } catch {
        Write-Host "    Decryption failed: $_" -ForegroundColor Red
        return $null
    }
}

# CN API endpoint (v1)
$BaseUrl = "https://launcher.hypergryph.com/api"
$AppCode = "6LL0KJuqHBVz33WK"  # Endfield CN
$LauncherAppCode = "abYeZZ16BPluCFyT"  # Endfield CN launcher

# Channel configs
$Channels = @(
    @{ Name = "CN-Official"; Channel = 1; SubChannel = 1 },
    @{ Name = "CN-Bilibili"; Channel = 2; SubChannel = 2 }
)

$Results = @{}
$GameFilesData = @{}

foreach ($Ch in $Channels) {
    Write-Host "Fetching $($Ch.Name) (channel=$($Ch.Channel), sub=$($Ch.SubChannel))..."

    $Params = @{
        appcode = $AppCode
        launcher_appcode = $LauncherAppCode
        channel = $Ch.Channel
        sub_channel = $Ch.SubChannel
        launcher_sub_channel = $Ch.SubChannel
    }

    try {
        $QueryParts = @()
        foreach ($Param in $Params.GetEnumerator()) {
            $QueryParts += "$($Param.Key)=$($Param.Value)"
        }
        $Url = "$BaseUrl/game/get_latest?$($QueryParts -join "&")"
        $Response = Invoke-RestMethod -Uri $Url -Method GET -TimeoutSec 30

        # Debug: Save raw response
        $Response | ConvertTo-Json -Depth 10 | Out-File -FilePath "tmp\$($Ch.Name)_response.json" -Encoding UTF8

        if ($Response.pkg) {
            $FileList = @()
            $Version = $Response.version

            if ($Response.pkg.packs) {
                $FileList = $Response.pkg.packs | ForEach-Object { "$($_.url.Split("/")[-1].Split("?")[0])|$($_.md5)" } | Sort-Object
            }

            $Results[$Ch.Name] = @{
                Files = $FileList
                Count = $FileList.Count
                Version = $Version
                GameFilesMd5 = $Response.pkg.game_files_md5
                FilesPath = $Response.pkg.file_path
            }

            Write-Host "  Found $($FileList.Count) packs, version: $Version" -ForegroundColor Green
            Write-Host "    game_files MD5: $($Response.pkg.game_files_md5)" -ForegroundColor Gray
        } else {
            Write-Host "  No pkg found in response" -ForegroundColor Yellow
            $Response | ConvertTo-Json -Depth 3 | Write-Host -ForegroundColor DarkGray
        }
    } catch {
        Write-Host "  ERROR: $_" -ForegroundColor Red
    }
}

# Compare if both channels fetched
if ($Results.Count -eq 2) {
    $Official = $Results["CN-Official"]
    $Bilibili = $Results["CN-Bilibili"]

    Write-Host "`n=== Pack Comparison Results ===" -ForegroundColor Cyan

    # Find common files
    $Common = $Official.Files | Where-Object { $Bilibili.Files -contains $_ }
    Write-Host "Common packs: $($Common.Count)" -ForegroundColor Yellow

    # Find differences
    $OnlyOfficial = $Official.Files | Where-Object { $Bilibili.Files -notcontains $_ }
    $OnlyBilibili = $Bilibili.Files | Where-Object { $Official.Files -notcontains $_ }

    Write-Host "Only in Official: $($OnlyOfficial.Count)" -ForegroundColor Magenta
    Write-Host "Only in Bilibili: $($OnlyBilibili.Count)" -ForegroundColor Magenta

    $OverlapPercent = [math]::Round(($Common.Count / $Official.Files.Count) * 100, 2)
    Write-Host "Pack overlap: $OverlapPercent%" -ForegroundColor Cyan

    # Compare game_files manifests
    Write-Host "`n=== Game Files Manifest Comparison ===" -ForegroundColor Cyan
    Write-Host "Official game_files MD5: $($Official.GameFilesMd5)" -ForegroundColor Gray
    Write-Host "Bilibili game_files MD5: $($Bilibili.GameFilesMd5)" -ForegroundColor Gray

    if ($Official.GameFilesMd5 -eq $Bilibili.GameFilesMd5) {
        Write-Host "game_files manifests are IDENTICAL" -ForegroundColor Green
    } else {
        Write-Host "game_files manifests are DIFFERENT" -ForegroundColor Yellow

        # Download and decrypt game_files for detailed comparison
        Write-Host "`n  Downloading and decrypting game_files manifests..." -ForegroundColor Cyan

        $OfficialFiles = $null
        $BilibiliFiles = $null

        try {
            $WebClient = New-Object System.Net.WebClient
            $OfficialBytes = $WebClient.DownloadData("$($Official.FilesPath)/game_files")
            Write-Host "    Official: Downloaded $($OfficialBytes.Length) bytes" -ForegroundColor Gray
            $OfficialFiles = Decrypt-GameFiles -EncryptedBytes $OfficialBytes
            if ($OfficialFiles) {
                Write-Host "    Official: Decrypted $($OfficialFiles.Count) files" -ForegroundColor Gray
            }
        } catch {
            Write-Host "    Official: Failed - $_" -ForegroundColor Red
        }

        try {
            $WebClient2 = New-Object System.Net.WebClient
            $BilibiliBytes = $WebClient2.DownloadData("$($Bilibili.FilesPath)/game_files")
            Write-Host "    Bilibili: Downloaded $($BilibiliBytes.Length) bytes" -ForegroundColor Gray
            $BilibiliFiles = Decrypt-GameFiles -EncryptedBytes $BilibiliBytes
            if ($BilibiliFiles) {
                Write-Host "    Bilibili: Decrypted $($BilibiliFiles.Count) files" -ForegroundColor Gray
            }
        } catch {
            Write-Host "    Bilibili: Failed - $_" -ForegroundColor Red
        }

        # Compare individual files if both decrypted successfully
        if ($OfficialFiles -and $BilibiliFiles) {
            Write-Host "`n=== Individual File Comparison ===" -ForegroundColor Cyan

            # Create hash sets for comparison
            $OffFileHashes = @{}
            $OfficialFiles | ForEach-Object { $OffFileHashes[$_.path] = $_.md5 }

            $BiliFileHashes = @{}
            $BilibiliFiles | ForEach-Object { $BiliFileHashes[$_.path] = $_.md5 }

            $CommonFiles = 0
            $DifferentFiles = 0
            $OnlyOfficialFiles = 0
            $OnlyBilibiliFiles = 0

            # Check Official files against Bilibili
            foreach ($Path in $OffFileHashes.Keys) {
                if ($BiliFileHashes.ContainsKey($Path)) {
                    if ($OffFileHashes[$Path] -eq $BiliFileHashes[$Path]) {
                        $CommonFiles++
                    } else {
                        $DifferentFiles++
                    }
                } else {
                    $OnlyOfficialFiles++
                }
            }

            # Check Bilibili-only files
            foreach ($Path in $BiliFileHashes.Keys) {
                if (-not $OffFileHashes.ContainsKey($Path)) {
                    $OnlyBilibiliFiles++
                }
            }

            $TotalFiles = $OfficialFiles.Count
            $FileOverlap = [math]::Round(($CommonFiles / $TotalFiles) * 100, 2)

            Write-Host "Total files in Official: $TotalFiles" -ForegroundColor Gray
            Write-Host "Total files in Bilibili: $($BilibiliFiles.Count)" -ForegroundColor Gray
            Write-Host "Identical files (path+hash): $CommonFiles ($FileOverlap%)" -ForegroundColor $(if($FileOverlap -gt 90){"Green"}else{"Yellow"})
            Write-Host "Different hash (same path): $DifferentFiles" -ForegroundColor $(if($DifferentFiles -eq 0){"Green"}else{"Yellow"})
            Write-Host "Only in Official: $OnlyOfficialFiles" -ForegroundColor Gray
            Write-Host "Only in Bilibili: $OnlyBilibiliFiles" -ForegroundColor Gray

            # Summary
            Write-Host "`n=== Summary ===" -ForegroundColor Cyan
            Write-Host "FINDING: Pack files are channel-specific (0% overlap), but" -ForegroundColor Yellow
            Write-Host "         extracted game files are nearly identical ($FileOverlap% overlap)" -ForegroundColor Yellow
            Write-Host "`nImplications for launcher implementation:" -ForegroundColor Gray
            Write-Host "  - Independent directories: Simple, no conflicts" -ForegroundColor Gray
            Write-Host "  - Future optimization: File-level deduplication could save ~90GB" -ForegroundColor Gray
            Write-Host "  - Server switch: Symlink change (fast, no re-download)" -ForegroundColor Gray
        }
    }

    # Save diff to file
    $DiffOutput = @()
    $DiffOutput += "=== Channel Cross-Compatibility Comparison ==="
    $DiffOutput += "Date: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
    $DiffOutput += ""
    $DiffOutput += "=== Pack Comparison ==="
    $DiffOutput += "Official packs: $($Official.Count)"
    $DiffOutput += "Bilibili packs: $($Bilibili.Count)"
    $DiffOutput += "Common: $($Common.Count)"
    $DiffOutput += "Overlap: $OverlapPercent%"
    $DiffOutput += ""
    $DiffOutput += "=== game_files Manifest ==="
    $DiffOutput += "Official MD5: $($Official.GameFilesMd5)"
    $DiffOutput += "Bilibili MD5: $($Bilibili.GameFilesMd5)"

    if ($OfficialFiles -and $BilibiliFiles) {
        $DiffOutput += ""
        $DiffOutput += "=== Individual File Comparison ==="
        $DiffOutput += "Official files: $($OfficialFiles.Count)"
        $DiffOutput += "Bilibili files: $($BilibiliFiles.Count)"
        $DiffOutput += "Identical (path+hash): $CommonFiles ($FileOverlap%)"
        $DiffOutput += "Different hash: $DifferentFiles"
        $DiffOutput += "Only in Official: $OnlyOfficialFiles"
        $DiffOutput += "Only in Bilibili: $OnlyBilibiliFiles"
        $DiffOutput += ""
        $DiffOutput += "=== Summary ==="
        $DiffOutput += "FINDING: Pack files are channel-specific (0% overlap),"
        $DiffOutput += "         but extracted game files are nearly identical ($FileOverlap% overlap)"
        $DiffOutput += ""
        $DiffOutput += "Implications:"
        $DiffOutput += "  - Independent directories recommended (simple, no conflicts)"
        $DiffOutput += "  - Future: File-level deduplication could save ~90GB"
        $DiffOutput += "  - Server switch: Symlink change (fast, no re-download)"
    }

    $DiffOutput += ""
    $DiffOutput += "=== Pack Differences ==="
    $DiffOutput += "Only in Official ($($OnlyOfficial.Count)):"
    $DiffOutput += $OnlyOfficial | ForEach-Object { "  - $($_.Split("|")[0])" }
    $DiffOutput += ""
    $DiffOutput += "Only in Bilibili ($($OnlyBilibili.Count)):"
    $DiffOutput += $OnlyBilibili | ForEach-Object { "  - $($_.Split("|")[0])" }

    $DiffOutput | Out-File -FilePath "tmp\channel_diff.txt" -Encoding UTF8
    Write-Host "`nDiff saved to tmp\channel_diff.txt"
}

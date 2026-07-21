# API_PROTOCOL — Download Protocol, URL Patterns, CDN Mapping

Part of the Hypergryph Official API Reference. See [`API.md`](API.md) for the full index.

---

## 1. Split Zip Volume Format

### File Naming
```
<base_name>.zip.001
<base_name>.zip.002
...
<base_name>.zip.NNN
```

### Format facts
- Each volume is typically ~2.2 GB (last volume may be smaller)
- Endfield v1.1.9 has **39 volumes** for all channels (CN official, CN Bilibili, Global official)
- Parts must be read sequentially as a single concatenated stream
- The resulting combined stream is a standard zip archive

---

## 2. Multi-Volume Extraction

### Process
1. Open all volume files in sequential order
2. Read the zip central directory from the end of the combined stream (last volume contains the end-of-central-directory record)
3. Extract each entry to the target game directory
4. **No physical merge needed** — stream volumes directly during extraction

### Implementation Notes
- The C# reference uses a `MultiVolumeStream` class that presents multiple `.zip.NNN` files as a single `Stream`
- In Rust, implement a similar wrapper around `std::fs::File` handles for each volume
- Track current volume index and offset within the current volume
- Delegate `Read` trait to the active volume file

---

## 3. Integrity Verification (4 Levels)

### Level 1: Pack-level MD5
| What | Source | When to Check |
|------|--------|---------------|
| Each downloaded `.zip.NNN` volume | `pkg.packs[].md5` / `patch.patches[].md5` from batch API | After each volume download completes |

**Process**: After downloading each `.zip.NNN` file, compute MD5 and compare against the pack's `md5` field from the API response.

### Level 2: Volume Count
| What | Source | When to Check |
|------|--------|---------------|
| Number of downloaded volumes matches API response | Length of `pkg.packs[]` array | Before extraction begins |

**Process**: Ensure all expected volumes are present and accounted for before starting extraction.

### Level 3: Game Files Manifest (`game_files`)
| What | Source | When to Check |
|------|--------|---------------|
| Encrypted manifest with per-file MD5 and size | `{file_path}/game_files` from API | After extraction AND for repair verification |

**Process**:
1. Download `game_files` from CDN (e.g., `https://beyond.hg-cdn.com/.../files/game_files`)
2. Verify its MD5 matches `rsp.pkg.game_files_md5` from API
3. Decrypt using AES-256-CBC (see [`API_RESOURCES.md` §4](API_RESOURCES.md))
4. Parse JSON Lines format to get expected file list
5. Compare each local file's MD5 against the manifest

**Example entry**:
```json
{"path":"Endfield.exe","md5":"d35b68cf269b794e7c273b44efa2b7a2","size":827864}
```

### Level 4: Individual File Repair
| What | Source | When to Check |
|------|--------|---------------|
| Corrupted/missing individual files | CDN direct download | When manifest comparison shows differences |

**Process**:
When the manifest comparison reveals files with mismatched MD5s:

1. Delete the corrupted local file
2. Download individual file from CDN using pattern:
   ```
   {file_path}/{filename}
   ```
   Example:
   ```
   https://beyond.hg-cdn.com/YDUTE5gscDZ229CW/1.1/update/6/6/Windows/1.1.9_PPTMaQzu2HpbgY7P/files/EndfieldBase.dll
   ```
3. The CDN returns the file with `Content-MD5` HTTP header for verification

**Telemetry observed**:
- `integrity_repair_start` with `repair_files: N` (number of corrupted files)
- `launcher_src_finish` with `"error_msg": "md5 file check success"`

---

## 4. HTTP Resume Support

### Range Requests
- CDN servers support the `Range` header for byte-range requests
- Partial downloads can resume by tracking bytes received per volume
- Format: `Range: bytes=<start>-`

### Implementation
- Track `bytes_received` per volume file
- On resume, open existing partial file, get its size, and send `Range: bytes=<size>-`
- Append response body to the partial file

---

## 5. URL Pattern Reference

### Complete Game Update URL Template

```
Base: https://<cdn_domain>/<appCode>/{major}.{minor}/update/{channel}/{subChannel}/Windows/{version}_{randStr}/

Packs:   {base}/packs/<name>.zip.001 through .zip.NNN
Patches: {base}/patches/{source_version}/{name}.zip.001 through .zip.NNN
Res:     {base}/res/{platform}/{major}.{minor}/{res_version}/index_main.json
                               index_initial.json
                               patch.json
                               Patch/{patch_filename}
Files:   {base}/files.json (zst compressed manifest)
```

### Resource Files URL Template

```
{base}/res/{platform}/{major}.{minor}/{res_version}/
  |- index_main.json        (encrypted, base64-encoded)
  |- index_initial.json     (encrypted, base64-encoded)
  |- patch.json             (plain JSON)
  |- Patch/{patch_filename} (binary patch files)
```

### Launcher Update URL Template

```
CN ZIP:     https://launcher-sign.hycdn.cn/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.zip
CN EXE:     https://launcher-rule.hycdn.cn/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.exe
OS ZIP:     https://launcher.hg-cdn.com/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.zip
OS EXE:     https://launcher-rule.hg-cdn.com/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.exe
```

---

## 6. CDN Domain Mapping

**Region convention**: CN = `hypergryph.com` / `hycdn.cn`, OS = `gryphline.com` / `hg-cdn.com`

### Game CDN Domains

| Domain | Region | Used For | Auth Keys |
|--------|--------|----------|-----------|
| `beyond.hycdn.cn` | CN | Endfield game packs, patches, resources | Yes (`?auth_key=...`) |
| `beyond.hg-cdn.com` | Global | Endfield game packs, patches, resources | No |
| `ak.hycdn.cn` | CN | Arknights game packs, patches | Yes (`?auth_key=...`) |
| `ak.hg-cdn.com` | Global | Arknights game packs, patches | No |

### Launcher CDN Domains

| Domain | Region | Used For | Auth Keys |
|--------|--------|----------|-----------|
| `launcher-sign.hycdn.cn` | CN | Launcher ZIP (CN) | Yes (`?auth_key=...`) |
| `launcher-rule.hycdn.cn` | CN/OS | Launcher EXE (both) | Yes (`?auth_key=...`) |
| `launcher.hg-cdn.com` | OS | Launcher ZIP (OS) | No |
| `launcher.gryphline.com` | OS | Launcher API | No |
| `launcher.hypergryph.com` | CN | Launcher API (CN) | No |

### Auth Key Behavior
- CN URLs contain `?auth_key=<timestamp_signature>` query parameters
- These expire after a TTL (estimated 1-4 hours)
- **Always fetch fresh URLs from the batch API before starting a download session**
- Do NOT cache pack URLs across sessions — they will be invalid

**TODO**: Find the exact auth-key lifetime before URLs are cached.

---

## 7. ak ↔ beyond URL Interchangeability

The URL structure between Arknights and Endfield is **nearly identical**. There are two substitution dimensions:

### 1. Game prefix: `ak` ↔ `beyond`

| Aspect | Arknights | Endfield |
|--------|-----------|----------|
| Batch API gateway (CN) | `launcher.hypergryph.com` (confirmed) | `beyond-gw.hypergryph.com` |
| Batch API gateway (OS) | — | `beyond-gw.gryphline.com` |
| Account/game server (OS) | — | `u8.gryphline.com` |
| Game CDN (CN) | `ak.hycdn.cn` | `beyond.hycdn.cn` |
| Game CDN (OS) | `ak.hg-cdn.com` | `beyond.hg-cdn.com` |
| Request/response schema | Same | Same |
| Pack split format | Confirmed | Confirmed (`.zip.NNN`) |

### 2. Region suffix: `hypergryph.com` ↔ `gryphline.com`

| Aspect | CN | OS |
|--------|----|----|
| Domain root | `*.hypergryph.com` | `*.gryphline.com` |
| CDN | `*.hycdn.cn` (auth keys) | `*.hg-cdn.com` (no auth) |
| Launcher API | `launcher.hypergryph.com` | `launcher.gryphline.com` |

**Implication**: A single generic API client with `game` (`ak` / `beyond`) and `region` (`cn` / `os`) identifiers can handle all combinations by swapping domain prefixes.

---

## 8. Patch URL Patterns

### Delta Patch Structure

```
{base}/patches/{source_version}/{name}.zip.001
{base}/patches/{source_version}/{name}.zip.002
...
```

The `{source_version}` subdirectory indicates which version the patch upgrades **from**. For example:
- `patches/1.0.13/` — patches from v1.0.13 to current
- `patches/1.0.14/` — patches from v1.0.14 to current

### Key Observations
- Patches are ALSO split volumes (`.zip.001`, `.zip.002`, etc.), not single files
- The official launcher may offer multiple patch paths depending on version gap
- **TODO**: Find the best patch chain (minimum bytes or fewer downloads)

---

## 9. Channel Compatibility Findings (Endfield CN)

Empirical comparison (Endfield CN 1.1.9) shows:

| Metric | Pack Level | Individual File Level (`game_files` MD5) |
|--------|------------|-------------------------------------------|
| Common | 0 (0%) | 1,121 (99.73%) |
| Differences | 39 unique packs/channel | 3 (Official-only) + 71 (Bilibili-only) |
| Totals | 39 packs/channel | 1,124 (Official) / 1,192 (Bilibili) |

Interpretation:
- Pack archives are channel-specific.
- Extracted file sets are highly overlapping.
- Current project strategy remains independent install roots per server; reuse/dedup is an optimization, not a requirement.

Research helper used for this comparison:
- `scripts/research/compare_channels.ps1`

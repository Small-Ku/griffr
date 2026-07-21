# API_CORE — Batch API & Game Update API

Part of the Hypergryph Official API Reference. See [`API.md`](API.md) for the full index.

---

## 1. Batch API (Core Gateway)

Both games use a single POST batch endpoint that multiplexes multiple sub-requests in one call.

### Endpoints

**Pattern: SG = `gryphline.com`, CN = `hypergryph.com`**

```
Arknights CN:   POST https://launcher.hypergryph.com/api/proxy/batch_proxy   (confirmed from Hi3Helper)
Arknights SG:   no known official PC target (the gateway exists, but no preset/appcode is documented)
Endfield CN:    POST https://launcher.hypergryph.com/api/proxy/batch_proxy   (same endpoint as Arknights CN)
Endfield SG:    POST https://launcher.gryphline.com/api/proxy/batch_proxy   (confirmed from ak-endfield-api-archive)
```

**Note**: The implementation uses `launcher.hypergryph.com` for Endfield CN and `launcher.gryphline.com` for Endfield SG.

### Other API bases found in `config.ts` base64 values

| Key | Decoded | Region | Purpose |
|-----|---------|--------|---------|
| `launcher` | `launcher.gryphline.com/api` | SG | Endfield Overseas launcher API |
| `launcherCN` | `launcher.hypergryph.com/api` | CN | Endfield China launcher API |
| `u8` | `u8.gryphline.com` | SG | Account-to-game-server token exchange & server list |
| `accountService` | `as.gryphline.com` | SG | Overseas account service |
| `gameHub` | `game-hub.gryphline.com` | SG | Game hub (gift code redeem) |
| `binding` | `binding-api-account-prod.gryphline.com` | SG | Account binding |
| `webview` | `ef-webview.gryphline.com` | SG | Endfield webview |
| `zonai` | `zonai.skport.com` | SG | Payment gateway |

**Note**: All `config.ts` base entries are SG/overseas (`gryphline.com`). CN equivalents use `hypergryph.com`.

### Confirmed from Hi3Helper (Arknights CN)

| Setting | Value |
|---------|-------|
| API URL | `https://launcher.hypergryph.com/api/proxy/batch_proxy` |
| Web API URL | `https://launcher.hypergryph.com/api/proxy/web/batch_proxy` |
| Game app code | `GzD1CpaWgmSq1wew` |
| Launcher app code | `abYeZZ16BPluCFyT` |
| Channel (官服) | `1` / `1` |
| Channel (B服) | `2` / `2` |
| Seq | `5` |

For Endfield CN, decrypted local `config.ini` and the archived Bilibili pack URLs agree with the same launcher tuples: CN Official = `1/1`, CN Bilibili = `2/2`.

### Request Format

```json
{
  "seq": "<request sequence ID>",
  "proxy_reqs": [
    {
      "kind": "get_latest_game",
      "get_latest_game_req": {
        "appcode": "<app_code>",
        "channel": "<channel_id>",
        "sub_channel": "<sub_channel_id>",
        "version": "<current_installed_version_or_empty>",
        "launcher_appcode": "<launcher_app_code>"
      }
    },
    {
      "kind": "get_banner",
      "get_banner_req": { "appcode": "...", "language": "zh-cn", "channel": "1", "sub_channel": "1", "platform": "Windows", "source": "launcher" }
    },
    {
      "kind": "get_announcement",
      "get_announcement_req": { "appcode": "...", "language": "zh-cn", "channel": "1", "sub_channel": "1", "platform": "Windows", "source": "launcher" }
    },
    {
      "kind": "get_main_bg_image",
      "get_main_bg_image_req": { "appcode": "...", "language": "zh-cn", "channel": "1", "sub_channel": "1", "platform": "Windows", "source": "launcher" }
    },
    {
      "kind": "get_sidebar",
      "get_sidebar_req": { "appcode": "...", "language": "zh-cn", "channel": "1", "sub_channel": "1", "platform": "Windows", "source": "launcher" }
    }
  ]
}
```

### Response Format

```json
{
  "proxy_rsps": [
    {
      "kind": "get_latest_game",
      "get_latest_game_rsp": { ... }
    },
    {
      "kind": "get_banner",
      "get_banner_rsp": { ... }
    }
  ]
}
```

Match responses by `kind` field to correlate with requests.

---

## 2. Game Update API (`get_latest_game`)

Returns version info and download pack lists for full installs and delta updates.

### Request Parameters

| Field | Type | Description |
|-------|------|-------------|
| `appcode` | string | Game-specific app code (see [`API_CONFIG.md`](API_CONFIG.md)) |
| `channel` | string | Channel ID (see [`API_CONFIG.md`](API_CONFIG.md)) |
| `sub_channel` | string | Sub-channel ID (see [`API_CONFIG.md`](API_CONFIG.md)) |
| `version` | string | Current installed version. Empty for fresh install, or existing version for delta check |
| `launcher_appcode` | string | Launcher app code (see [`API_CONFIG.md`](API_CONFIG.md)) |

### Response Structure

```json
{
  "action": 0 | 1 | 2,
  "request_version": "1.0.14",
  "version": "1.1.9",
  "pkg": {
    "packs": [
      {
        "url": "https://<cdn>/<path>/packs/<name>.zip.001",
        "md5": "<hex_md5>",
        "package_size": "<size_bytes_string>"
      }
    ],
    "total_size": "<total_bytes_string>",
    "file_path": "https://<cdn>/<path>/files.json",
    "game_files_md5": "<optional_manifest_md5>"
  },
  "patch": {
    "patches": [
      {
        "url": "https://<cdn>/<path>/patches/<name>.zip.001",
        "md5": "<hex_md5>",
        "package_size": "<size_bytes_string>"
      }
    ],
    "total_size": "<total_patch_size_string>",
    "package_size": "<alternative_total_string>"
  },
  "state": 0,
  "launcher_action": 0
}
```

### Action Codes

| Code | Meaning |
|------|---------|
| 0 | No update needed (up to date) |
| 1 | Update available — full install packs in `pkg` |
| 2 | Patch available — delta patches in `patch` |

### Pack URL Patterns

**Full install packs** (split zip volumes):
```
https://<cdn>/<appCode>/{major}.{minor}/update/{channel}/{subChannel}/Windows/{version}_{randStr}/packs/<name>.zip.001
https://<cdn>/<appCode>/{major}.{minor}/update/{channel}/{subChannel}/Windows/{version}_{randStr}/packs/<name>.zip.002
...
```

**Delta patch packs** (also split volumes, not single files!):
```
https://<cdn>/<appCode>/{major}.{minor}/update/{channel}/{subChannel}/Windows/{version}_{randStr}/patches/{source_version}/{name}.zip.001
...
```

**Note**: Patch pack URLs follow a slightly different pattern depending on the version gap (e.g., `patches/1.0.13/`, `patches/1.0.14/` subdirectories).

### Key Observations

- **Packs are split volumes** (`.zip.001`, `.zip.002`, etc.), typically ~2.2 GB each
- **Patch packs are ALSO split** — not single files like some other game launchers
- **MD5 is mandatory** — every pack has its own `md5` field for post-download verification
- **File manifest** at `{file_path}/game_files` provides per-file MD5 map. It is AES-256-CBC encrypted. See [`API_RESOURCES.md` §4](API_RESOURCES.md) for decryption details.
- **`request_version`** field in response shows the version you sent (useful for delta update correlation)

---

## 3. Account Session Persistence (Current Windows Runtime Paths)

Launcher-related account/session artifacts are observed in three buckets.

### A) Per-user LocalLow SDK/MMKV state (primary)

Observed under:

```
%USERPROFILE%\AppData\LocalLow\Hypergryph\Endfield\sdk_data_*
%USERPROFILE%\AppData\LocalLow\Gryphline\Endfield\sdk_data_*
%USERPROFILE%\AppData\LocalLow\Hypergryph\Arknights\sdk_data_*
```

Common files:

```
login_cache
gf_login_cache
gameprotocol_cache
mmkv.default
*.crc
```

These are opaque MMKV blobs and are bound to the current Windows user profile path, so different OS users on the same PC do not share this state by default.

Reference implementation evidence: `ref/Xel-Launcher-master/Helpers/GameLauncher.cs` backs up/restores exactly `LocalLow\Hypergryph\{Game}\sdk_data_*` for account switching.

### B) Install-local `mmkv\` directories (secondary/variant)

Some installs also include:

```
{InstallRoot}\mmkv\
```

This appears to be launcher/runtime-variant cache state and is not reliable as the sole account source across channels/builds.

### C) Launcher WebView cache stores

Observed under:

```
%LOCALAPPDATA%\Games\{hash}\cache_storage\
  Cookies
  Local Storage\leveldb
  Session Storage
```

These hold launcher WebView/browser state and cached payloads. They are not a stable contract for token parsing.

### Practical implication for third-party launchers

For account switching, the safest scoped strategy is snapshot/restore of per-user `LocalLow\Hypergryph\{Game}\sdk_data_*` as opaque blobs, with optional install-local `mmkv\` capture as compatibility fallback. Current `griffr-cli` workflow is explicit path-in/path-out bundles (no central account registry directory yet).

Current `griffr-cli` account command caveat: server identity is not auto-inferred from SDK payload. Commands are game-scoped and can narrow default root selection with `--region-hint cn|sg` (`cn` -> `Hypergryph`, `sg` -> `Gryphline`); without a hint they scan both and pick the latest modified `sdk_data_*` unless `--sdk-dir` is provided explicitly.

See [`DESIGN_account_model.md`](DESIGN_account_model.md) for the concrete `griffr` model (storage layout, switching semantics, and security constraints).

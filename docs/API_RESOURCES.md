# API_RESOURCES — Res Index, Encryption, File Manifest

Part of the Hypergryph Official API Reference. See [`API.md`](API.md) for the full index.

---

## 1. Game Resources API (VFS / Res Index)

Returns resource pack info for game assets (audio, textures, etc.) — separate from main game packs.

Scope note (validated 2026-05-01):
- This endpoint is confirmed working for **Endfield**.
- For **Arknights** CN official and CN bilibili, live calls return `400 INVALID_PARAM resource not exist` with fresh `get_latest_game`-derived parameters.
- In current project behavior, Arknights should not use this VFS planning path.

### Endpoint

Launcher API endpoint (direct GET, not batch):
```
CN: GET https://launcher.hypergryph.com/api/game/get_latest_resources
OS: GET https://launcher.gryphline.com/api/game/get_latest_resources
```

Query params:
```
appcode=<game_appcode>
game_version=<major.minor>   # e.g. 1.2
version=<full_version>       # e.g. 1.2.5
platform=Windows
rand_str=<randStr>
```

**Important**: `/api` is required in the path. `.../game/get_latest_resources` (without `/api`) returns HTTP 404.

The resource base URL is derived from the `get_latest_game` response:
```
https://<cdn_domain>/<appCode>/{major}.{minor}/update/{channel}/{subChannel}/Windows/{version}_{randStr}/
```

Inside this base, resource files are at:
```
{base}/res/{platform}/{major}.{minor}/{res_version}/index_main.json
{base}/res/{platform}/{major}.{minor}/{res_version}/index_initial.json
{base}/res/{platform}/{major}.{minor}/{res_version}/patch.json
{base}/res/{platform}/{major}.{minor}/{res_version}/Patch/{patch_filename}
```

---

## 2. Resource Index Encryption

Resource index files (`index_main.json`, `index_initial.json`) are **NOT simple XOR encrypted**. The actual algorithm is a modular subtraction cipher.

### Decryption Algorithm (from `cipher.ts`)

For each byte at position `i`:
```
plain_byte[i] = (enc_byte[i] - key_byte[i % key_length] + 256) % 256
```

### Encryption (Inverse)

For each byte at position `i`:
```
enc_byte[i] = (plain_byte[i] + key_byte[i % key_length]) % 256
```

### Key (Endfield, from `config.ts`)

```
resIndexKey = "Assets/Beyond/DynamicAssets/Gameplay/UI/Fonts/"
```

### Full Decryption Process

1. Base64 decode the raw `.json` file content
2. Apply modular subtraction cipher with the UTF-8 key bytes (cycled)
3. Result is UTF-8 JSON text

### Arknights Key

This does not apply to the Arknights path. Tests have not shown that Arknights serves this VFS resource API.

---

## 3. Resource Index JSON Structure

### `index_main.json` / `index_initial.json`

```json
{
  "version": "<res_version>",
  "path": "<res_base_url>",
  "files": [
    {
      "name": "<filename>",
      "size": <bytes>,
      "md5": "<hex_md5_or_null>",
      "hash": "<optional_hash_string>"
    }
  ]
}
```

Notes:
- `md5` can be `null` in live payloads.
- Clients should accept nullable strings in index JSON.
- Checksum fallback order should be `md5` then `hash`; entries with neither checksum should be skipped.

### Local Runtime Semantics (Observed on Endfield OS, 2026-04-25)

Empirical filesystem observation after wiping both `Persistent` and `StreamingAssets`, running official launcher integrity, then launching the game:

- `StreamingAssets` was the launcher baseline file target.
  - Integrity plan (`download_sdk_config`) listed:
    - `713` VFS files under `Endfield_Data/StreamingAssets/VFS/*`
    - plus `index_initial.json` and `index_main.json`
    - (`715` total entries)
- After first game init (`Assets initialized`), `Persistent` contained:
  - `index_initial.json`, `pref_initial.json`, and exactly `10` VFS files.
  - `index_main.json` / `pref_main.json` were not present yet.
  - `pref_initial.files` matched the `Persistent\VFS` set exactly.
  - `index_initial.files` had `12` entries, with 2 non-pref files left in `StreamingAssets`.
- After in-game resource download completion, `Persistent` expanded to:
  - `index_initial.json`, `pref_initial.json`, `index_main.json`, `pref_main.json`
  - `90` VFS files total
  - exact set match: `Persistent\VFS == (pref_initial.files ∪ pref_main.files)`.

Practical interpretation:

- `index_*.json` is the full candidate set.
- `pref_*.json` lists the selected files that the game writes to `Persistent`.
- Absence from `pref_*` is the effective "not selected" signal.
- There is no explicit per-file reason field in `index_*` or `pref_*`.
- File-check rule:
  - If `pref_*` exists, `Persistent` should be treated as `pref-only` scope.
  - `index-full` scope should be used for `StreamingAssets` baseline validation, not `Persistent`.
  - If `game_files` and `index_*` collide on a `StreamingAssets` destination, the resource index owns validation and repair.
- The game selects these files. The reason for each choice is not known. Follow the file set that the game selects.

### `patch.json`

```json
{
  "files": [
    {
      "md5": "<current_file_md5>",
      "size": <bytes>,
      "patch": [
        {
          "base_md5": "<previous_file_md5>",
          "patch": "<patch_filename>",
          "patch_size": <bytes>
        }
      ]
    }
  ]
}
```

**Patch chain**: `md5Old → patch → md5New`. Multiple patches per file allow incremental updates across versions. Patch files are located at `{res_base}/Patch/{patch_filename}`.

---

## 4. File Manifest (`game_files`)

The `game_files` manifest contains the complete list of game files with their MD5 hashes and sizes. It is **AES-256-CBC encrypted**.

### Download URL

```
{file_path}/game_files
```

Where `{file_path}` is the `pkg.file_path` URL from the batch API response. For example:
```
https://beyond.hg-cdn.com/YDUTE5gscDZ229CW/1.1/update/6/6/Windows/1.1.9_PPTMaQzu2HpbgY7P/files/game_files
```

### Verification

The batch API response includes `rsp.pkg.game_files_md5` — an MD5 hash of the **encrypted** manifest file. Verify the downloaded manifest matches this hash before decryption.

### Encryption Details

| Property | Value |
|----------|-------|
| Algorithm | AES-256-CBC |
| Key | `C0F30E1CE763BBC21CC355A34303AC50399444BFF68C4A22AF398C0A166EE143` |
| IV | `33467861192750649501937264608400` |
| Padding | PKCS7 |

### Decryption (C#)

```csharp
using System;
using System.IO;
using System.Security.Cryptography;
using System.Text;

public static class ArknightsCrypto
{
    private static readonly byte[] AesKey = new byte[]
    {
        0xC0, 0xF3, 0x0E, 0x1C, 0xE7, 0x63, 0xBB, 0xC2, 0x1C, 0xC3, 0x55, 0xA3, 0x43, 0x03, 0xAC, 0x50,
        0x39, 0x94, 0x44, 0xBF, 0xF6, 0x8C, 0x4A, 0x22, 0xAF, 0x39, 0x8C, 0x0A, 0x16, 0x6E, 0xE1, 0x43
    };

    private static readonly byte[] AesIv = new byte[]
    {
        0x33, 0x46, 0x78, 0x61, 0x19, 0x27, 0x50, 0x64, 0x95, 0x01, 0x93, 0x72, 0x64, 0x60, 0x84, 0x00
    };

    public static string DecryptGameFiles(string filePath)
    {
        var fileBytes = File.ReadAllBytes(filePath);
        using var aes = Aes.Create();
        aes.Key = AesKey;
        aes.IV = AesIv;
        aes.Mode = CipherMode.CBC;
        aes.Padding = PaddingMode.PKCS7;

        using var decryptor = aes.CreateDecryptor();
        var decryptedBytes = decryptor.TransformFinalBlock(fileBytes, 0, fileBytes.Length);
        return Encoding.UTF8.GetString(decryptedBytes);
    }
}
```

### Decrypted Format

After decryption, the file contains **JSON Lines** (one JSON object per line). Example:

```json
{"path":"api-ms-win-core-console-l1-1-0.dll","md5":"07ebe4d5cef3301ccf07430f4c3e32d8","size":12240}
{"path":"api-ms-win-core-console-l1-2-0.dll","md5":"57193bfbccefe3d5df8c1a0d27c4e8d4","size":12256}
{"path":"Endfield.exe","md5":"d35b68cf269b794e7c273b44efa2b7a2","size":827864}
...
```

Each entry contains:
- `path`: Relative file path from game root
- `md5`: MD5 hash of the file (hex string, 32 chars)
- `size`: File size in bytes

**Note**: The Endfield v1.1.9 manifest contains 1,124 file entries.

### Source

The AES key and IV were extracted from `ArknightsCrypto.cs` in the Hi3Helper.Plugin.Arknights project. The same encryption is used for both Arknights and Endfield manifests.

---

## 5. Arknights Resource Delivery Note

Arknights appears to deliver asset updates through package/patch payloads (including AB-oriented layout) and `game_files` integrity manifests rather than Endfield-style VFS `get_latest_resources` sync.

This should be treated as an implementation constraint unless future captures prove otherwise.

---

## 6. Mirror File List (Alternative CDN)

The archive includes `mirror_file_list.json` which maps CDN domain aliases to their primary domains. Useful for CDN fallback and failover.

**TODO**: Verify the mirror list is still current and implement CDN fallback logic.

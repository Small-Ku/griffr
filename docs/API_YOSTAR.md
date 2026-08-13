# YoStar Arknights KR/EN/JP Launcher Update Protocol

This document records reverse-engineered behavior observed from the YoStar Arknights KR, EN, and JP Electron launcher bundles inspected in August 2026. All three report launcher version **1.8.1** and share the same game-update protocol and implementation; their deployment identity is parameterized by gateway and `game_tag`.

| Region | Gateway | `game_tag` | Native install identity |
|---|---|---|---|
| KR | `https://api-launcher-kr.yo-star.com` | `Arknights_KR` | `Arknights_KR` |
| EN | `https://api-launcher-en.yo-star.com` | `Arknights_EN` | `Arknights_EN` |
| JP | `https://api-launcher-jp.yo-star.com` | `Arknights_JP` | `Arknights_JP` |

Griffr models these as one YoStar protocol family with a region-specific target, not three providers.

## Request authorization

Game API calls attach an `Authorization` JSON value containing a `head` and MD5 signature. The observed launchers use the same signing salt in all three regions:

```text
head = {
  game_tag: <region game_tag>,
  time: <unix timestamp>,
  version: "1.8.1"
}

sign = MD5(JSON.stringify(head) + request_body + "DE7108E9B2842FD460F4777702727869")
```

The deployment-specific `game_tag` is therefore part of request identity even though the signing algorithm and salt are shared.

## Launcher self-update

The launcher uses `electron-updater` (6.1.1) with automatic download disabled, install-on-quit enabled, and the web installer disabled. Launcher self-update and game-content update are separate systems.

The updater feed is not obtained from the game configuration endpoint. `electron-updater` reads installed resources such as `app-update.yml`; those external Electron resources were not present in the inspected ASARs, so this reference does not establish each region's self-update feed URL.

Before installing a downloaded launcher update, the launcher protects the sibling `YostarGames` directory by stopping executables found below it, moving the game directory out of the updater's installation scope, and recording the temporary location under `%TEMP%`. The temporary filename incorporates the native regional identity, for example `temp-Arknights_EN.txt`.

## Game metadata API

The main game metadata call is:

```text
GET <region gateway>/api/launcher/game/config
```

Observed response fields include:

| Field | Meaning |
|---|---|
| `game_latest_version` | Latest published game version. |
| `game_lowest_version` | Oldest version still permitted to launch. |
| `game_latest_file_path` | Manifest basis/locator associated with the latest version. |
| `game_start_exe_name` | Executable persisted into local launcher metadata. |
| `game_start_params` | Launch arguments persisted into local launcher metadata. |
| `game_uninstall_script` | Launcher-owned uninstall script path/name. |
| `decompression_size` | Space estimate shown by the launcher UI. |

`game_latest_version` and `game_lowest_version` have different semantics. The official launcher can permit an installed version that is at least `game_lowest_version` even when it is behind `game_latest_version`.

## Manifest and CDN resolution

A concrete manifest is selected by both version and basis:

```text
GET /api/launcher/game/config/json?version=<version>&file_path=<basis>
```

The response supplies the URL of the actual manifest, so release identity is at least:

```text
(version, basis)
```

The manifest contains a source prefix and file entries equivalent to:

```json
{
  "source": "...",
  "file": [
    {
      "path": "...",
      "size": 123,
      "hash": "<CRC64-XZ>"
    }
  ]
}
```

CDN roots come from:

```text
GET /api/launcher/advanced/game/download/cdn
```

which returns `primary_cdn` and `back_up_cdn`. A payload URL is formed from the selected CDN root, manifest `source`, and file `path`.

## Local metadata and regional detection

The launcher writes two relevant files:

```text
manifest.json
game-launcher-config.json
```

`manifest.json` records:

```text
name
version
basis
vc
files[] { path, size, hash, vc }
```

`game-launcher-config.json` records:

```text
tag
name
params
version
gameUninstallScript
vc
```

For KR/EN/JP, both `manifest.name` and launcher `tag` use the corresponding native `Arknights_KR`, `Arknights_EN`, or `Arknights_JP` identity. Griffr uses this native tag as the source of truth when detecting an existing YoStar installation's region.

The file `hash` is the expected CRC64-XZ payload checksum. `vc` is a local metadata consistency value calculated as base64-encoded MD5 over serialized object values; it is not an authenticity signature.

## Normal update vs full repair

YoStar's normal update is manifest-diff driven. It obtains a canonical manifest for the installed `(version, basis)` and the target manifest, then classifies paths:

```text
same path + same hash -> unchanged
new path              -> download
same path + new hash  -> download
old-only path         -> delete
```

For files classified unchanged by the manifests, normal update checks local existence and size instead of initially recomputing CRC64 across the entire installation. Full repair computes CRC64 for every target-manifest file and repairs missing or mismatched content.

The official launcher performs another target-manifest integrity pass before committing newly downloaded content, so same-size corruption that escapes the initial fast update comparison can still be detected during final verification.

## File transfer and commit

The observed game updater materializes target files individually:

```text
manifest diff
  -> download <target>.tmp
  -> HTTP Range resume when a partial exists
  -> CRC64-XZ verification
  -> rename temporary file to final path
```

It downloads up to ten files concurrently and retries across primary/backup CDN roots. Network interruption can preserve a partial `.tmp` for Range resume; checksum failure discards the invalid temporary payload.

Native metadata is prepared under temporary names and committed after target verification. The official sequence is staged but not a filesystem-wide transaction: obsolete files can be deleted before the final verification/rename phase. Griffr intentionally keeps its own persisted change barrier and conservative ownership rules rather than copying this weakness.

## Comparison with Hypergryph/Gryphline

| Capability | YoStar Arknights KR/EN/JP | Hypergryph/Gryphline observed by Griffr |
|---|---|---|
| Release routing | Region-specific `game_tag` | Game/launcher app codes plus channel/sub-channel. |
| Latest metadata | Latest + minimum launchable version + manifest basis | `get_latest_game` target version plus `pkg`, `patch`, `pre_patch`. |
| Minimum launchable version | Explicit `game_lowest_version` | No equivalent field observed. |
| Target manifest identity | `(version, basis)` | `(version, normalized pkg.file_path, game_files_md5)`. |
| Manifest payload | JSON with CRC64-XZ file entries | Encrypted `game_files`, MD5-verified before decrypt. |
| Direct file source | CDN root + manifest source/path | `pkg.file_path` plus `/files/<path>` mapping. |
| Full archives | Not used by observed updater path | Multipart full ZIP packs. |
| Delta patch | Not observed | Official patch archives / HDiff flow. |
| Predownload | Not observed | `pre_patch` is first-class delivery metadata. |
| Resource/VFS layer | File manifest is observed content authority | Endfield additionally exposes resource-index/VFS APIs. |

The useful design lesson is to keep **desired content state** separate from **delivery providers**. Full archives, official patches, local reuse, direct CDN files, and VFS patch inputs are alternative ways to materialize the desired state; YoStar simply exposes a different subset of those mechanisms.

## Griffr implementation

`arknights + kr`, `arknights + en`, and `arknights + jp` all resolve to the same YoStar backend. Explicit channel/sub-channel selectors are rejected because this protocol routes the title by regional `game_tag`, not Hypergryph-style channel IDs.

The implemented lifecycle is:

- `install`: resolve the latest `(version, basis)` manifest, materialize files from primary/backup CDN or validated local reuse, verify CRC64-XZ, then commit native region-specific YoStar metadata.
- `update`: resolve installed and target canonical manifests when possible, stat-check unchanged entries, verify/materialize changed entries, conservatively handle obsolete paths, and commit metadata last. If the old canonical manifest can no longer be resolved, Griffr does not delete paths whose prior server ownership cannot be proven.
- `verify`: operate offline from validated local YoStar metadata and hash every selected core file with CRC64-XZ. `--repair` additionally resolves canonical manifest/CDN data and rematerializes failures.
- `launch`: honor Griffr's `.griffr/state.json` change barrier and perform the official-style existence/size quick check before launching with persisted arguments.
- `info` and `uninstall`: detect all three regions from native YoStar metadata. On Windows, uninstall can run a present, safely named launcher uninstall batch hook before owned-root cleanup.

The observed protocol does not expose Hypergryph-style `pre_patch` staging or Endfield Persistent/VFS APIs. Griffr therefore rejects those operations for YoStar rather than fabricating compatibility semantics.

## Debug protocol probes

The dedicated debug subtree exposes the native YoStar protocol without forcing it through Hypergryph/Gryphline command semantics:

```bash
griffr debug yostar config --region kr
griffr debug yostar cdn --region en
griffr debug yostar manifest --region jp
griffr debug yostar manifest --region jp --version <VERSION> --basis <BASIS>
griffr debug yostar file-url --region en --file <MANIFEST_PATH>
```

`config` calls the game configuration endpoint, `cdn` returns primary/backup CDN metadata, `manifest` resolves and validates either the latest or an explicit `(version, basis)` manifest, and `file-url` resolves one manifest entry to its candidate CDN URLs. `--gateway` can redirect these probes to a compatible fixture/server for testing.

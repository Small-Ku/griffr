# YoStar Arknights EN Launcher Update Protocol

This document records reverse-engineered behavior observed from the YoStar Arknights EN Electron launcher bundle inspected in August 2026. It is a protocol/design reference, not a promise that the service remains unchanged.

The inspected launcher application reports version **1.8.1**. Its own self-update mechanism and the game-content updater are separate systems.

## Launcher self-update

The launcher uses `electron-updater` (6.1.1) with automatic download disabled and install-on-quit enabled. Startup asks the updater for a newer launcher before normal game initialization continues. If an update is available, the normal UI flow is gated until the launcher update is downloaded and installed.

The updater feed is not obtained from YoStar's game configuration endpoint. `electron-updater` reads the installed `resources/app-update.yml`; that external resource was not present in the unpacked application bundle that was inspected, so the provider/feed URL is not established by this reference.

Before installing a downloaded launcher update, the launcher protects the sibling `YostarGames` directory by stopping executables found below it, renaming the game directory out of the updater's installation scope, and recording the temporary location in `%TEMP%/temp-Arknights_EN.txt`. The next launcher process restores the directory.

## Game metadata API

The main game metadata call is:

```text
GET https://api-launcher-en.yo-star.com/api/launcher/game/config
```

Observed consumers establish at least these response fields:

| Field | Meaning |
|---|---|
| `game_latest_version` | Latest published game version. |
| `game_lowest_version` | Oldest version still permitted to launch. |
| `game_latest_file_path` | Manifest basis/locator associated with the latest version. |
| `game_start_exe_name` | Executable persisted into local launcher metadata. |
| `game_start_params` | Launch arguments persisted into local launcher metadata. |
| `game_uninstall_script` | Launcher-owned uninstall script path/name. |
| `decompression_size` | Space estimate shown by the launcher UI. |

`game_latest_version` and `game_lowest_version` have different semantics. The launcher permits a locally installed version when it is greater than or equal to `game_lowest_version`; the settings UI can still report that it is behind `game_latest_version`. A newly published version therefore does not have to be an immediate mandatory update.

## Manifest resolution

The latest metadata does not itself contain the file list. The launcher resolves a concrete manifest using both version and basis:

```text
GET /api/launcher/game/config/json?version=<version>&file_path=<basis>
```

The response supplies a URL for the actual manifest. A release's manifest identity is therefore at least:

```text
(version, basis)
```

rather than version alone.

The game manifest contains a source prefix and file entries with at least:

```json
{
  "source": "...",
  "file": [
    {
      "path": "...",
      "size": 123,
      "hash": "<CRC64>"
    }
  ]
}
```

A separate CDN configuration endpoint returns primary and backup roots. A file URL is formed from the CDN root, manifest `source`, and file `path`.

## Local metadata

The inspected launcher writes two relevant local files.

`manifest.json` records the installed manifest identity and file expectations:

```text
name
version
basis
vc
files[] { path, size, hash, vc }
```

`hash` is the CRC64 expected for the payload. `vc` is a local metadata consistency value calculated as base64-encoded MD5 over serialized object values; it is not an authenticity signature.

`game-launcher-config.json` records launcher UX/launch state including:

```text
tag
name
params
version
gameUninstallScript
vc
```

The version is consequently duplicated between launcher-oriented metadata and manifest-oriented metadata. Normal successful commits update both together.

## Normal update vs full repair

YoStar's normal update is manifest-diff driven. It obtains a canonical manifest for the installed `(version, basis)` and the target manifest, then classifies paths:

```text
same path + same hash -> unchanged
new path              -> download
same path + new hash  -> download
old-only path         -> delete
```

For files classified unchanged by the manifests, normal update checks local existence and size rather than recomputing CRC64 for the entire installation. This deliberately separates update work from a full integrity audit.

Full repair instead computes CRC64 for all target-manifest files and repairs missing or mismatched content. A same-size local corruption can therefore survive a normal update/quick check but is detected by full repair.

## File transfer and commit

The inspected game updater materializes individual target files rather than replaying a mandatory version-by-version patch chain:

```text
manifest diff
  -> download <target>.tmp
  -> HTTP Range resume when a partial exists
  -> CRC64 while/after receiving the file
  -> rename temporary file to final path
```

The downloader uses up to ten concurrent files. It can alternate between primary and backup CDN roots during retry.

Launcher metadata is first prepared under temporary names. The launcher performs a target-manifest integrity pass over changed temporary files and retained files, then renames successful payloads and metadata into place. This is a staged commit, but it is not a filesystem-wide transaction or journal.

## Comparison with Hypergryph/Gryphline APIs

The protocol observed by Griffr differs substantially from YoStar's delivery model.

| Capability | YoStar Arknights EN | Hypergryph/Gryphline observed by Griffr |
|---|---|---|
| Release routing | `game_tag` | game/launcher app codes plus channel/sub-channel. |
| Latest metadata | latest + minimum launchable version + manifest basis | `get_latest_game` target version plus `pkg`, `patch`, `pre_patch`. |
| Minimum launchable version | Explicit `game_lowest_version` | No equivalent field observed. |
| Target manifest identity | `(version, basis)` | `(version, normalized pkg.file_path, game_files_md5)`. |
| Manifest payload | JSON with CRC64 file entries | Encrypted `game_files`, MD5-verified before decrypt. |
| Direct file source | CDN root + manifest source/path | `pkg.file_path` plus `/files/<path>` mapping. |
| Full archives | Not used by the observed game updater path | Multipart full ZIP packs. |
| Delta patch | Not observed | Official patch archives / HDiff flow. |
| Predownload | Not observed | `pre_patch` is first-class delivery metadata. |
| Resource/VFS layer | File manifest is the observed content authority | Endfield also exposes `get_latest_resources` and resource-index/VFS work. |

The useful design lesson is not to imitate YoStar's downloader. Griffr already has stronger providers such as multipart ZIP range extraction, hardlink/copy reuse, direct CDN repair, patch DAGs, and VFS synchronization. The useful lesson is to keep **desired content state** separate from **delivery providers**.

A target manifest should define what the installation must contain. Full archives, official patches, local reuse, direct CDN files, and VFS patch inputs are alternative ways to materialize that desired state, selected according to correctness constraints and cost rather than treated as mutually exclusive update modes.

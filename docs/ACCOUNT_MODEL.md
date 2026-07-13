# Account Storage Model (Windows)

This document records observed account/session persistence behavior on this machine and defines the scoped account model for `griffr`.

## 1. Observed Launcher Storage (Empirical)

### 1.1 Per-User LocalLow SDK/MMKV Session Caches (Primary)

Observed under:

- `%USERPROFILE%\AppData\LocalLow\Hypergryph\Endfield\sdk_data_*`
- `%USERPROFILE%\AppData\LocalLow\Gryphline\Endfield\sdk_data_*` (observed on SG/global launcher variants)
- `%USERPROFILE%\AppData\LocalLow\Hypergryph\Arknights\sdk_data_*`

Common files:

- `login_cache`
- `gf_login_cache`
- `gameprotocol_cache`
- `mmkv.default`
- corresponding `.crc` files

Notes:

- Files are opaque MMKV blobs; `griffr` does not parse token payloads.
- This path is user-profile scoped, so different Windows users on the same PC do not share sessions by default.
- `ref\Xel-Launcher-master\Helpers\GameLauncher.cs` uses backup/restore of exactly these `sdk_data_*` directories for Arknights and Endfield account switching.

### 1.2 Install-Local `mmkv` Caches (Secondary / Variant)

Observed in some game roots:

- `G:\Games\Library\Hypergryph\Bili\EndField Game\mmkv\`
- `G:\Games\Library\Hypergryph\Bili\Arknights Game\mmkv\`
- `G:\Games\Library\Hypergryph\CN\Arknights Game\mmkv\`

Presence is inconsistent across channel/build combinations (for example, not observed in current `CN\Endfield Game` install root). Treat as optional compatibility cache, not the primary source of account identity.

### 1.3 Launcher WebView Cache Buckets

Observed under `%LOCALAPPDATA%\Games\{hash}\cache_storage\`:

- `Cookies` (SQLite)
- `Local Storage\leveldb`
- `Session Storage`

Observed content includes launcher media/cache data keyed by app codes (`6LL0KJuqHBVz33WK`, `YDUTE5gscDZ229CW`, `GzD1CpaWgmSq1wew`), but no reliable plain-text token contract should be assumed.

### 1.4 Roaming Launcher Folders

Observed under `%APPDATA%`:

- `Hypergryph\{hash}\icon\...`
- `Gryphline\{hash}\icon\...`

These appear to be launcher UI assets/metadata, not the primary account session source.

## 2. Scoped Account Model for `griffr`

`griffr` treats account/session state as opaque launcher blobs and operates at directory/file granularity only.

### 2.1 Storage Scope

- No central `griffr` account registry directory is used in this phase.
- Account bundles are explicit user-provided paths passed on each command invocation.
- Bundle shape:
  - `{bundle}\sdk_data\...` (required)
  - `{bundle}\mmkv\...` (optional compatibility payload from install-root `mmkv\`)

### 2.2 Switching Semantics

- `griffr account capture <game> --to <bundle>`:
  - copies per-user `LocalLow\...\sdk_data_*` into `{bundle}\sdk_data`
  - optionally includes install-root `mmkv\` via `--include-install-mmkv --install-path <path>`
- `griffr account activate <game> --from <bundle> --force`:
  - restores `{bundle}\sdk_data` into target `LocalLow\...\sdk_data_*`
  - optionally restores `{bundle}\mmkv` into install-root `mmkv\` when requested
- No stored labels/metadata/index are maintained by `griffr`; users organize bundle locations externally.

Current region-selection behavior:

- `griffr account` commands are game-scoped only; they do not infer launcher region from SDK payload.
- If `--sdk-dir` is omitted, source/target defaults to the most recently modified `sdk_data_*` directory in roots selected by `--region-hint`:
  - `--region-hint cn` -> `LocalLow\Hypergryph\{Game}`
  - `--region-hint sg` -> `LocalLow\Gryphline\{Game}`
  - no `--region-hint` -> scan both roots and pick latest modified
- For deterministic per-server workflows on machines with multiple logged-in server profiles, users should pass `--sdk-dir` explicitly and maintain their own bundle naming convention (for example, `endfield_cn_official`, `endfield_cn_bilibili`).

`griffr` will not modify launcher WebView cache buckets by default in this phase.

### 2.3 Security Constraints

- No token parsing or reserialization.
- No network upload/sync of bundle data.
- No plaintext token logging.
- Explicit overwrite intent (`--force`) is required when replacing existing target directories.

### 2.4 Non-Goals (Current Scope)

- Cross-machine portability guarantees.
- Decrypting/auth-introspecting MMKV entries.
- Automatic account merge across channels/servers.
- Managing official launcher login UI sessions in `Cookies`/WebView stores.

## 3. Verification Evidence

Validated by filesystem inspection commands:

- Enumerated `%USERPROFILE%\AppData\LocalLow\Hypergryph\{Game}\sdk_data_*` trees and confirmed launcher-named MMKV cache files.
- Confirmed `Gryphline` vendor path compatibility for default SDK discovery in `crates/griffr-cli/src/commands/account.rs`.
- Cross-checked `ref\Xel-Launcher-master\Helpers\GameLauncher.cs` and confirmed account backup/restore targets LocalLow `sdk_data_*`.
- Enumerated install-root `mmkv` trees and confirmed they exist for some channels/builds but are not universal.
- Enumerated `%LOCALAPPDATA%\Games\{hash}\cache_storage` and confirmed WebView state stores.
- Enumerated `%APPDATA%\Hypergryph` and `%APPDATA%\Gryphline` and observed icon-centric metadata.
- Confirmed CLI behavior in `crates/griffr-cli/src/commands/account.rs`: default SDK resolution picks latest modified `sdk_data_*` and does not include server-id inference logic.

These checks establish that per-user LocalLow `sdk_data_*` is the stable primary account/session source for current Windows runtime paths.

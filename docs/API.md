# Hypergryph Official API Reference

Documented API structure for Arknights (明日方舟) and Arknights: Endfield (终末地) official PC launchers. The two launchers share some infrastructure (batch gateway, channels, package delivery), but resource pipelines are not fully interchangeable.

---

## File Index

| File | Contents |

|------|----------|

| [`API_CORE.md`](API_CORE.md) | Batch API gateway, Game Update API (`get_latest_game`), action codes, pack URL patterns |

| [`API_RESOURCES.md`](API_RESOURCES.md) | Res Index API, encryption algorithms (modular subtraction cipher, AES-256-CBC), `game_files` manifest decryption |

| [`API_MEDIA.md`](API_MEDIA.md) | Web resources — banners, announcements, background images, sidebar, locale support |

| [`API_LAUNCHER.md`](API_LAUNCHER.md) | Launcher update API, ZIP/EXE package responses, launcher domain mapping |

| [`API_CONFIG.md`](API_CONFIG.md) | Channel/server configuration tables, app codes, sub_channel mappings, User-Agent requirements |

| [`API_PROTOCOL.md`](API_PROTOCOL.md) | Download protocol — split zip volumes, multi-volume extraction, MD5 verification, individual file repair, HTTP resume, CDN mapping, `ak` ↔ `beyond` interchangeability |

---

## Quick Reference

### Core Batch Endpoint

**Pattern: SG = `gryphline.com`, CN = `hypergryph.com`**

| Game | Endpoint |
|------|----------|
| Arknights CN | `POST https://launcher.hypergryph.com/api/proxy/batch_proxy` |
| Arknights SG | No known official PC target/preset |
| Endfield CN | `POST https://launcher.hypergryph.com/api/proxy/batch_proxy` |
| Endfield SG | `POST https://launcher.gryphline.com/api/proxy/batch_proxy` |

**Web / Media endpoint** for banners, announcements, and sidebar:

| Region | Endpoint |
|--------|----------|
| CN | `POST https://launcher.hypergryph.com/api/proxy/web/batch_proxy` |
| SG | `POST https://launcher.gryphline.com/api/proxy/web/batch_proxy` |

### Additional discovered API bases

| Key | Decoded | SG/CN | Purpose |
|-----|---------|-------|---------|
| `launcher` | `launcher.gryphline.com/api` | SG | Endfield Overseas launcher API |
| `launcherCN` | `launcher.hypergryph.com/api` | CN | Endfield China launcher API |
| `u8` | `u8.gryphline.com` | SG | Account-to-game-server token exchange & server list (SG only) |
| `accountService` | `as.gryphline.com` | SG | Overseas account service |
| `gameHub` | `game-hub.gryphline.com` | SG | Game hub (gift code redeem) |
| `binding` | `binding-api-account-prod.gryphline.com` | SG | Account binding |
| `webview` | `ef-webview.gryphline.com` | SG | Endfield webview |
| `zonai` | `zonai.skport.com` | SG | Payment gateway |

### VFS Resources Endpoint

Direct launcher endpoint (not batch):

| Region | Endpoint |
|--------|----------|
| CN | `GET https://launcher.hypergryph.com/api/game/get_latest_resources` |
| SG | `GET https://launcher.gryphline.com/api/game/get_latest_resources` |

Requires query params: `appcode`, `game_version` (`major.minor`), `version`, `platform`, `rand_str`.

`/api` in the path is mandatory; omitting it returns HTTP 404.

### Resource Endpoint Scope (Validated)

Validated with live probes on **2026-05-01**:
- `get_latest_resources` is valid for **Endfield** (CN official and CN bilibili tested).
- The same call pattern returns `400 INVALID_PARAM resource not exist` for **Arknights** (CN official and CN bilibili tested), even with fresh `get_latest_game`-derived `version` + `rand_str`.

Practical implication for this project:
- Treat VFS `get_latest_resources` sync as **Endfield-only**.
- Arknights update/integrity should rely on package + `game_files`/AB delivery flow.

**Confirmed from Hi3Helper (Arknights CN)**:
- API URL: `https://launcher.hypergryph.com/api/proxy/batch_proxy`
- Web API URL: `https://launcher.hypergryph.com/api/proxy/web/batch_proxy`
- Arknights app code: `GzD1CpaWgmSq1wew`
- Launcher app code: `abYeZZ16BPluCFyT` (shared with Endfield CN)

**Full App Code Reference**:

| Component | Arknights CN | Endfield CN | Endfield SG | Endfield SG Epic |
|-----------|--------------|-------------|-------------|------------------|
| Game | `GzD1CpaWgmSq1wew` | `6LL0KJuqHBVz33WK` | `YDUTE5gscDZ229CW` | — |
| Launcher | `abYeZZ16BPluCFyT` | `abYeZZ16BPluCFyT` | `TiaytKBUIEdoEwRT` | `BBWoqCzuZ2bZ1Dro` |
| Account Service | — | — | `d9f6dbb6bbd6bb33` | — |
| SKPort | — | — | `6eb76d4e13aa36e6` | — |
| Binding API | — | — | `3dacefa138426cfe` | — |
| U8 | — | — | `973bd727dd11cbb6ead8` | — |

### CDN Domains
| Domain | Region | Auth Key |
|--------|--------|----------|
| `ak.hycdn.cn` | Arknights CN | Yes |
| `beyond.hycdn.cn` | Endfield CN | Yes |
| `beyond.hg-cdn.com` | Endfield SG | No |

### Native Region / Channel IDs
| Server | `channel` | `sub_channel` | `game appcode` |
|--------|-----------|---------------|----------------|
| CN Official | `1` | `1` | `6LL0KJuqHBVz33WK` / `GzD1CpaWgmSq1wew` |
| CN Bilibili | `2` | `2` | `6LL0KJuqHBVz33WK` / `GzD1CpaWgmSq1wew` |
| SG Official | `6` | `6` | `YDUTE5gscDZ229CW` |
| SG Epic | `6` | `801` | `YDUTE5gscDZ229CW` |
| SG Google Play | `6` | `802` | `YDUTE5gscDZ229CW` |

These tuples are confirmed from decrypted `config.ini` files in local CN/Bilibili installs for both Arknights and Endfield, and Endfield Bilibili pack URLs in `ref/ak-endfield-api-archive-main` also use `.../update/2/2/...`.

### Resource Index Decryption Key (Endfield)
```
resIndexKey = "Assets/Beyond/DynamicAssets/Gameplay/UI/Fonts/"
```

Algorithm: For each byte at position `i`:
```
plain_byte[i] = (enc_byte[i] - key_byte[i % key_length] + 256) % 256
```

Process: Base64 decode → modular subtraction cipher → UTF-8 JSON.

### Game Files Manifest Decryption Key (AES-256-CBC)
Used for `game_files` integrity manifest (both Arknights and Endfield):

```
Key: C0F30E1CE763BBC21CC355A34303AC50399444BFF68C4A22AF398C0A166EE143
IV:  33467861192750649501937264608400
```

Process: Download → MD5 verify → AES-256-CBC decrypt (PKCS7) → JSON Lines parse

### Split Pack Format
All game packs and patches are split zip volumes: `.zip.001` through `.zip.NNN` (~2.2 GB each, 39 volumes for Endfield v1.1.9).

### Latest Known Versions
- **Endfield**: v1.1.9 (39 packs, ~85 GB)
- **Endfield Launcher**: v1.2.2
- **Arknights**: Confirmed from Hi3Helper — app code `GzD1CpaWgmSq1wew`, launcher app code `abYeZZ16BPluCFyT`, channels 1/1 (官服) and 2/2 (B服)

### ak ↔ beyond Interchangeability

Domain-prefix substitution (`ak` ↔ `beyond`) is only partially valid.

- Core batch APIs and package/update structures are similar.
- Resource APIs are game-specific in practice: Endfield uses `get_latest_resources`; Arknights does not appear to expose the same VFS resource endpoint behavior.

Full comparison table: see [`API_PROTOCOL.md` §7](API_PROTOCOL.md).

### Batch API `seq` Field
- Type: Integer
- Scope: Per-session event counter
- Behavior: Monotonically incrementing (+1 per event)
- Fixed value `"1"` works for API requests; telemetry uses incrementing values

### User-Agent Requirements
| Minimum | `Mozilla/5.0` |
|---------|---------------|
| Official | `Mozilla/5.0 (Windows NT 6.2; Win64; x64)...QtWebEngine/5.15.8...PC/WIN/HGSDK HGWebPC/1.30.1` |

The minimum `Mozilla/5.0` is accepted by the batch API gateway.

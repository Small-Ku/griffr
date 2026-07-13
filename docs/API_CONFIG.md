# API_CONFIG — Channel Configuration, App Codes, Server Tables

Part of the Hypergryph Official API Reference. See [`API.md`](API.md) for the full index.

---

## 1. Domain Convention

**SG** (the official `config.ini` region value for overseas/global builds) → domains use `gryphline.com`
**CN** → domains use `hypergryph.com`

This is the single most important naming convention to get right. Every `*gryphline.com` endpoint has a `*hypergryph.com` CN equivalent.

---

## 2. Discovered API Base URLs (from `config.ts` base64 values)

All `config.ts` base entries resolve to SG/overseas (`gryphline.com`) domains. CN equivalents use `hypergryph.com`.

| Key | Decoded | Region | Purpose |
|-----|---------|--------|---------|
| `launcher` | `launcher.gryphline.com/api` | SG | Endfield Overseas launcher API |
| `launcherCN` | `launcher.hypergryph.com/api` | CN | Endfield CN launcher API |
| `u8` | `u8.gryphline.com` | SG | Account-to-game-server token exchange & server list |
| `accountService` | `as.gryphline.com` | SG | Overseas account service |
| `gameHub` | `game-hub.gryphline.com` | SG | Game hub (gift code redeem) |
| `binding` | `binding-api-account-prod.gryphline.com` | SG | Account binding/linking API |
| `webview` | `ef-webview.gryphline.com` | SG | Endfield webview content |
| `zonai` | `zonai.skport.com` | SG | Payment gateway |

---

## 3. App Codes

### Arknights (明日方舟) — Confirmed from Hi3Helper and decrypted local `config.ini`

These values are **verified from `ArknightsCnPresetConfig.cs`** in the Collapse Launcher plugin:

| Setting | Value |
|---------|-------|
| Game app code | `GzD1CpaWgmSq1wew` |
| Launcher app code | `abYeZZ16BPluCFyT` (shared with Endfield CN) |
| API URL | `https://launcher.hypergryph.com/api/proxy/batch_proxy` |
| Web API URL | `https://launcher.hypergryph.com/api/proxy/web/batch_proxy` |
| Seq | `5` |

| Server | `channel` | `sub_channel` | Notes |
|--------|-----------|---------------|-------|
| CN Official (官服) | `1` | `1` | Confirmed from local install metadata |
| CN Bilibili (B服) | `2` | `2` | Confirmed from local install metadata |
| SG | — | — | No official Arknights PC target is currently known |

### Endfield — Confirmed from `config.ts`, decrypted local `config.ini`, and `ak-endfield-api-archive`

#### Game App Codes
```
osWinRel  = "YDUTE5gscDZ229CW"   (SG/overseas Windows Release)
cnWinRel  = "6LL0KJuqHBVz33WK"   (CN Windows Release)
```

#### Launcher App Codes
```
osWinRel     = "TiaytKBUIEdoEwRT"  (SG)
osWinRelEpic = "BBWoqCzuZ2bZ1Dro"  (SG Epic)
cnWinRel     = "abYeZZ16BPluCFyT"  (CN — shared with Arknights CN)
```

#### Account App Codes
```
osWinRel = "d9f6dbb6bbd6bb33"    (SG)
skport   = "6eb76d4e13aa36e6"    (SK Port / Zenless)
binding  = "3dacefa138426cfe"    (Account binding)
```

#### U8 App Code
```
osWinRel = "973bd727dd11cbb6ead8"  (used for u8 token exchange)
```

This is **NOT** the batch API app code. The u8 service handles authentication token exchange and game server listing, not game version queries.

---

## 4. U8 Account/Game Server Service

`u8.gryphline.com` is a separate **authentication bridge** service — NOT part of the batch API. It manages the token exchange flow between the account service and the game servers.

### Endpoints

| Path | Method | Purpose |
|------|--------|---------|
| `/u8/user/auth/v2/token_by_channel_token` | POST | Exchange channel OAuth token → u8 access token |
| `/u8/user/auth/v2/grant` | POST | OAuth 2.0 grant |
| `/game/server/v1/server_list` | POST | List available game servers (ID, name, domain, port) |
| `/game/role/v1/confirm_server` | POST | Confirm server availability/accessibility |

### Auth Flow

1. Login via `as.gryphline.com` → get account service token
2. Get OAuth 2.0 code from account service
3. Exchange OAuth code → u8 token via `/u8/user/auth/v2/token_by_channel_token`
   - Request: `{ "appCode": "973bd727dd11cbb6ead8", "channelMasterId": 6, "type": 0, "platform": 2 }`
4. Use u8 token for: server list, server confirm, gift code, gacha record queries

### Token Exchange Request (`token_by_channel_token`)

```json
{
  "appCode": "973bd727dd11cbb6ead8",
  "channelMasterId": 6,
  "channelToken": "{\"code\":\"<oauth_code>\",\"type\":1,\"isSuc\":true}",
  "type": 0,
  "platform": 2
}
```

**Note**: Only SG (`gryphline.com`) — no CN variant has been discovered for u8.

---

## 5. Channel / Server Configuration

### Arknights: Endfield

| Server | `channel` | `sub_channel` | Game `appcode` | Launcher `appcode` | Notes |
|--------|-----------|---------------|----------------|--------------------|-------|
| CN Official | `1` | `1` | `6LL0KJuqHBVz33WK` | `abYeZZ16BPluCFyT` | Standard CN |
| CN Bilibili | `2` | `2` | `6LL0KJuqHBVz33WK` | `abYeZZ16BPluCFyT` | Confirmed by local Bilibili `config.ini` and `.../update/2/2/...` archive URLs |
| SG Official | `6` | `6` | `YDUTE5gscDZ229CW` | `TiaytKBUIEdoEwRT` | Standard SG |
| SG Epic | `6` | `801` | `YDUTE5gscDZ229CW` | `BBWoqCzuZ2bZ1Dro` | Epic Games Store |
| SG Google Play | `6` | `802` | `YDUTE5gscDZ229CW` | `TiaytKBUIEdoEwRT` | Google Play cross |

**Observed native values**: decrypted local `config.ini` uses `6/802` for Google Play. Current built-in aliases therefore resolve SG Epic to `6/801` and SG Google Play to `6/802`.

### Arknights (明日方舟) — Confirmed from Hi3Helper and decrypted local `config.ini`

| Server | `channel` | `sub_channel` | Game `appcode` | Notes |
|--------|-----------|---------------|----------------|-------|
| CN Official (官服) | `1` | `1` | `GzD1CpaWgmSq1wew` | Verified from Hi3Helper and local install metadata |
| CN Bilibili (B服) | `2` | `2` | `GzD1CpaWgmSq1wew` | Verified from Hi3Helper and local install metadata |
| SG | — | — | — | No official Arknights PC target is currently known |

---

## 6. User-Agent Requirements

From `config.ts`, the official launcher sends specific User-Agent strings:

### Minimum UA (Confirmed Working)
```
Mozilla/5.0
```

The batch API gateway accepts the minimal `Mozilla/5.0` User-Agent. This is the minimum required for API requests.

### Full Qt-based UA (Official Launcher)
```
Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) QtWebEngine/5.15.8 Chrome/92.0.4515.159 PC/WIN/HGSDK HGWebPC/1.30.1 Safari/537.36
```

The official launcher uses a QtWebEngine-based UA with the `PC/WIN/HGSDK HGWebPC/1.30.1` suffix to identify itself.

### Alternative UAs (from config.ts)
| UA Type | String |
|---------|--------|
| Chrome Windows | `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36...` |
| curl | `curl/8.4.0` |
| iOS | `Mozilla/5.0 (iPhone; CPU iPhone OS 18_0...` |

---

## 7. Batch API `seq` Field

The `seq` field is a **per-session event sequence counter** used for telemetry and request correlation.

### Properties

| Property | Value |
|----------|-------|
| Type | Integer (number) |
| Scope | Per-session |
| Behavior | Monotonically incrementing |
| Initial value | Varies (observed starting at 3, 7, 10) |
| Increment | +1 per event |

### Usage

- **API Requests**: Fixed values like `"1"` work for batch API calls
- **Telemetry**: Uses incrementing values to track event sequence within a session

### Example

```json
{
  "seq": "1",
  "proxy_reqs": [ ... ]
}
```

For most API implementations, a fixed `seq` value is sufficient. The field is primarily used for telemetry correlation in the official launcher.

---

## 8. Platform Field

The `platform` field indicates the client platform for the request.

### Confirmed Values

| Value | Platform | Status |
|-------|----------|--------|
| `"Windows"` | Windows PC | **Confirmed Working** |
| `"Android"` | Android mobile | Possible |
| `"iOS"` | iOS mobile | Possible |
| `"PS"` | PlayStation | Possible (Endfield console?) |
| `"Xbox"` | Xbox | Possible |

### Notes

- All current documentation and testing is based on `platform: "Windows"`
- Non-Windows platforms may return different pack structures (e.g., different file lists, architecture-specific binaries)
- The API accepts platform values but their impact on responses is untested for non-Windows platforms

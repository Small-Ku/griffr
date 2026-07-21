# API_LAUNCHER — Launcher Update API

Part of the Hypergryph Official API Reference. See [`API.md`](API.md) for the full index.

---

## 1. Launcher Update API

Separate endpoints for updating the Hypergryph Launcher itself (not the game). These are also accessed through the batch API as sub-requests.

### API Kinds

| Kind | Purpose |
|------|---------|
| `get_latest_launcher` | Fetch latest launcher ZIP package |
| `get_latest_launcher_exe` | Fetch latest standalone launcher EXE |
| `get_launcher_protocol` | Fetch launcher agreement/ToS protocol version |

### Launcher App Names (from `launcher_appcode` field in requests)

| Target | CN (`channel=1`) | OS (`channel=6`) |
|--------|-----------------|-------------------|
| Endfield | `EndField` | `EndField` |
| Arknights | `Arknights` | — |
| Official (shared) | `Official` | `Official` |

---

## 2. ZIP Package Response (`get_latest_launcher`)

```json
{
  "action": 0 | 1,
  "version": "1.2.2",
  "zip_package_url": "https://<launcher_cdn>/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<launcher_name>.zip",
  "md5": "<hex_md5>",
  "package_size": "<bytes_string>",
  "total_size": "<bytes_string>",
  "description": "<optional_description>"
}
```

- **action = 0**: Launcher is up to date
- **action = 1**: Update available

---

## 3. EXE Response (`get_latest_launcher_exe`)

```json
{
  "version": "1.2.2",
  "exe_url": "https://<launcher_cdn>/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<launcher_name>.exe",
  "package_size": "<bytes_string>"
}
```

The EXE is a standalone self-updating launcher binary. Unlike the ZIP, it does not include an MD5 field — the launcher must verify the EXE some other way (possibly via a hash from a separate source).

---

## 4. Launcher Protocol API (`get_launcher_protocol`)

```json
{
  "dataVersion": "<version_string>",
  "protocol_url": "<url_to_protocol_html_or_json>"
}
```

Returns the current launcher protocol version. Used to display ToS/privacy policy update prompts to the user. Not required for game downloads or updates.

---

## 5. Launcher Domain Mapping

| Region | ZIP Domain | EXE Domain |
|--------|-----------|------------|
| CN | `launcher-sign.hycdn.cn` | `launcher-rule.hycdn.cn` |
| OS | `launcher.hg-cdn.com` | `launcher-rule.hg-cdn.com` |

### URL Template

```
CN ZIP:  https://launcher-sign.hycdn.cn/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.zip
CN EXE:  https://launcher-rule.hycdn.cn/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.exe
OS ZIP:  https://launcher.hg-cdn.com/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.zip
OS EXE:  https://launcher-rule.hg-cdn.com/<launcher_appcode>/launcher/{version}/{channel}/{subChannel}/{randStr}/<name>.exe
```

**Note**: The EXE domain is shared between CN and OS (`launcher-rule.hycdn.cn` for CN, `launcher-rule.hg-cdn.com` for OS).

---

## 6. Archived Launcher Responses

The archive contains pre-fetched launcher API responses at:
```
output/akEndfield/launcher/launcher/EndField/{channel}/latest.json
```

This includes real data for channel 1 (CN) and channel 6 (OS), including actual URLs, sizes, and MD5 hashes for verification during development.

---

## 7. Notes

- Launcher updates are **separate from game updates** — always check both
- The launcher ZIP contains the full launcher application; the EXE is a self-extracting installer
- CN URLs may contain `?auth_key=<timestamp_signature>` parameters that expire
- **TODO**: Verify if launcher EXE has a separate MD5 endpoint or if it's skipped

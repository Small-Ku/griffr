# API_MEDIA — Web Resources, News, Banners

Part of the Hypergryph Official API Reference. See [`API.md`](API.md) for the full index.

---

## 1. Web Resources API (Media / News / UI)

All media resources are fetched through the batch API endpoint using `common_req` sub-requests.

### API Kinds

| Kind | Response Key | Purpose |
|------|-------------|---------|
| `get_banner` | `banner` | Carousel banner images with jump URLs |
| `get_announcement` | `announcement` | Game announcements grouped by tabs |
| `get_main_bg_image` | `main_bg_image` | Main launcher background image/video |
| `get_sidebar` | `sidebar` | Sidebar icons/links (community, wiki, etc.) |
| `get_single_ent` | `single_ent` | Single entry info (version page, etc.) |

---

## 2. Common Request

Media requests use a **flattened** structure (fields directly in the request, not nested under `common_req`):

```json
{
  "appcode": "<app_code>",
  "language": "zh-cn | en-us | ja-jp | zh-tw | ko-kr | ...",
  "channel": "<channel_id>",
  "sub_channel": "<sub_channel_id>",
  "platform": "Windows",
  "source": "launcher"
}
```

### Endpoint

Media requests use the **web batch endpoint**:
```
CN: POST https://launcher.hypergryph.com/api/proxy/web/batch_proxy
OS: POST https://launcher.gryphline.com/api/proxy/web/batch_proxy
```

### Locale Support

| Game (CN) | Game (Global) |
|-----------|---------------|
| `zh-cn`, `zh-tw` | `en-us`, `ja-jp`, `ko-kr`, `fr-fr`, `de-de`, `es-mx`, `pt-br`, `id-id`, `th-th`, `vi-vn`, `ru-ru`, `ar-sa`, `tr-tr` |

**Note**: CN servers only respond to `zh-cn` and `zh-tw`. Global servers support the full list.

---

## 3. Banner Response

```json
{
  "kind": "get_banner",
  "get_banner_rsp": {
    "data_version": "",
    "banners": [
      {
        "id": "77",
        "url": "<image_url>",
        "md5": "<image_md5>",
        "jump_url": "<link_url>",
        "need_token": true
      }
    ]
  }
}
```

---

## 4. Announcement Response

```json
{
  "kind": "get_announcement",
  "get_announcement_rsp": {
    "data_version": "",
    "tabs": [
      {
        "tabName": "公告",
        "tab_id": "30",
        "announcements": [
          {
            "id": "133",
            "content": "公告标题",
            "jump_url": "<link_url>",
            "start_ts": "<unix_timestamp_ms>",
            "need_token": true
          }
        ]
      }
    ]
  }
}
```

Tab names vary by game/locale: `公告`, `活动`, `新闻`, `公告`, etc.

---

## 5. Background Image Response

```json
{
  "kind": "get_main_bg_image",
  "get_main_bg_image_rsp": {
    "data_version": "",
    "main_bg_image": {
      "url": "<image_url>",
      "md5": "<image_md5>",
      "video_url": "<video_url_or_empty_string>"
    }
  }
}
```

`video_url` is typically empty — the launcher uses the static image. When present, use the video as the main background.

---

## 6. Sidebar Response

```json
{
  "kind": "get_sidebar",
  "get_sidebar_rsp": {
    "data_version": "",
    "sidebars": [
      {
        "display_type": "DisplayType_RESERVE",
        "media": "Bilibili",
        "pic": {
          "url": "<icon_url>",
          "md5": "<icon_md5>",
          "description": "<text>"
        },
        "jump_url": "<link_url>",
        "sidebar_labels": [
          {
            "content": "<label>",
            "jump_url": "<url>",
            "need_token": true
          }
        ],
        "grid_info": null,
        "need_token": true
      }
    ]
  }
}
```

Note: `pic` can be `null` for some sidebar items (e.g., Bilibili link without custom icon).

Sidebar entries typically link to: official website, social media, wiki, community forums, payment pages.

---

## 7. Single Entry Response

```json
{
  "kind": "get_single_ent",
  "single_ent": {
    "version_url": "<url>",
    "...": "..."
  }
}
```

**TODO**: The full structure of `single_ent` is not yet mapped. It may contain version history, changelog links, or other single-page content.

---

## 8. Archived Web Resources (from `ak-endfield-api-archive`)

The archive contains pre-fetched web resource responses at:
```
output/akEndfield/launcher/web/{channel}/{resource}/{locale}/latest.json
```

This is useful for:
- Testing the response parsing without hitting the live API
- Understanding the exact JSON schema with real data
- Offline development/testing

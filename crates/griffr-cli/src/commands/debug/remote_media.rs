use std::path::PathBuf;

use anyhow::Result;
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId};
use serde_json::{json, Value};

use super::utils::emit_json;
use crate::GlobalOptions;

fn media_to_json(
    game_id: GameId,
    channel_id: ChannelPair,
    language: &str,
    media: &griffr_common::api::client::MediaResponse,
) -> Value {
    json!({
        "game": game_id.to_string(),
        "channel": channel_id.channel().to_string(),
            "sub_channel": channel_id.sub_channel().to_string(),
        "language": language,
        "banners": media.banners.as_ref().map(|b| {
            json!({
                "data_version": b.data_version,
                "items": b.banners.iter().map(|item| {
                    json!({
                        "id": item.id,
                        "url": item.url,
                        "md5": item.md5,
                        "jump_url": item.jump_url,
                        "need_token": item.need_token,
                    })
                }).collect::<Vec<_>>()
            })
        }),
        "announcements": media.announcements.as_ref().map(|a| {
            json!({
                "data_version": a.data_version,
                "tabs": a.tabs.iter().map(|tab| {
                    json!({
                        "tab_name": tab.tab_name,
                        "announcements": tab.announcements.iter().map(|item| {
                            json!({
                                "id": item.id,
                                "content": item.content,
                                "jump_url": item.jump_url,
                                "start_ts": item.start_ts,
                                "need_token": item.need_token,
                            })
                        }).collect::<Vec<_>>()
                    })
                }).collect::<Vec<_>>()
            })
        }),
        "background": media.background.as_ref().map(|bg| {
            json!({
                "data_version": bg.data_version,
                "main_bg_image": {
                    "url": bg.main_bg_image.url,
                    "md5": bg.main_bg_image.md5,
                    "video_url": bg.main_bg_image.video_url,
                }
            })
        }),
        "sidebar": media.sidebar.as_ref().map(|s| {
            json!({
                "data_version": s.data_version,
                "items": s.sidebars.iter().map(|item| {
                    json!({
                        "display_type": item.display_type,
                        "media": item.media,
                        "pic": item.pic.as_ref().map(|pic| json!({
                            "url": pic.url,
                            "md5": pic.md5,
                            "description": pic.description,
                        })),
                        "jump_url": item.jump_url,
                        "sidebar_labels": item.sidebar_labels.iter().map(|label| {
                            json!({
                                "content": label.content,
                                "jump_url": label.jump_url,
                            })
                        }).collect::<Vec<_>>(),
                        "grid_info": item.grid_info,
                        "need_token": item.need_token,
                    })
                }).collect::<Vec<_>>()
            })
        }),
    })
}

pub async fn api_get_media(
    game_id: GameId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    language: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target =
        griffr_common::config::resolve_api_target(&game_id, &channel_id, &overrides.into())?;
    let payload = api_client.get_media_raw(&target, &language).await?;
    emit_json(output, payload).await
}

pub async fn fetch_media(
    game_id: GameId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    language: String,
    output: Option<PathBuf>,
    _opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target =
        griffr_common::config::resolve_api_target(&game_id, &channel_id, &overrides.into())?;
    let media = api_client.get_media(&target, &language).await?;
    let payload = media_to_json(game_id, channel_id, &language, &media);
    emit_json(output, payload).await
}

use anyhow::Result;
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId};
use serde_json::json;

use crate::{ui, GlobalOptions, OutputFormat};

fn format_announcement_text(content: &str) -> String {
    ui::strip_html_tags(content)
}

pub async fn show(
    game_id: GameId,
    channel_id: ChannelPair,
    overrides: crate::ApiTargetOverrideArgs,
    language: &str,
    opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let target =
        griffr_common::config::resolve_api_target(&game_id, &channel_id, &overrides.into())?;
    let media = api_client.get_media(&target, language).await?;

    let announcements = media.announcements;
    if opts.output == OutputFormat::Json {
        let tabs = announcements
            .as_ref()
            .map(|a| {
                a.tabs
                    .iter()
                    .map(|tab| {
                        json!({
                            "tab": tab.tab_name,
                            "announcements": tab.announcements.iter().map(|item| {
                                json!({
                                    "id": item.id,
                                    "content": item.content,
                                    "content_text": format_announcement_text(&item.content),
                                    "jump_url": item.jump_url,
                                    "start_ts": item.start_ts,
                                    "start_time": ui::format_unix_ms(&item.start_ts),
                                })
                            }).collect::<Vec<_>>()
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        return ui::emit_json(&json!({
            "game": game_id.to_string(),
            "channel": channel_id.channel().to_string(),
            "sub_channel": channel_id.sub_channel().to_string(),
            "language": language,
            "tabs": tabs,
        }));
    }

    ui::print_kv_section(
        "News",
        &[
            ("game".to_string(), game_id.to_string()),
            ("channel".to_string(), channel_id.channel().to_string()),
            ("sub_channel".to_string(), channel_id.sub_channel().to_string()),
            ("language".to_string(), language.to_string()),
        ],
    );

    let Some(announcements) = announcements else {
        println!();
        ui::print_info("No announcement payload returned by API.");
        return Ok(());
    };

    for tab in announcements.tabs {
        println!();
        ui::print_info(format!("Tab: {}", tab.tab_name));
        if tab.announcements.is_empty() {
            ui::print_info("  (empty)");
            continue;
        }

        for (index, announcement) in tab.announcements.iter().enumerate() {
            let content = format_announcement_text(&announcement.content);
            println!("  {}. {}", index + 1, content);

            if let Some(ts) = ui::format_unix_ms(&announcement.start_ts) {
                ui::print_info(format!("     start: {}", ts));
            }
            if opts.verbose {
                ui::print_info(format!("     id: {}", announcement.id));
                ui::print_info(format!("     url: {}", announcement.jump_url));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_text_is_sanitized() {
        assert_eq!(
            format_announcement_text("<h1>Patch</h1><p>Live now</p>"),
            "PatchLive now"
        );
    }
}

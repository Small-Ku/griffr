use anyhow::Result;
use griffr_common::api::client::ApiClient;
use griffr_common::config::{GameId, ServerId};

use crate::GlobalOptions;

pub async fn show(
    game_id: GameId,
    server_id: ServerId,
    language: &str,
    opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let media = api_client.get_media(game_id, server_id, language).await?;

    println!(
        "news game={:?} server={} language={}",
        game_id, server_id, language
    );

    if let Some(announcements) = media.announcements {
        for tab in announcements.tabs {
            println!("tab={}", tab.tab_name);
            for announcement in tab.announcements {
                println!("announcement={}", announcement.content);
                if opts.verbose {
                    println!("url={}", announcement.jump_url);
                    println!("start_ts={}", announcement.start_ts);
                }
            }
        }
    }

    Ok(())
}

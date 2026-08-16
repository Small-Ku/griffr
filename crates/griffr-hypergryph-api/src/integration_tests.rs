//! Integration tests against real Hypergryph API channels
//!
//! These tests make actual network requests and are marked with `#[ignore]`
//! so they only run when explicitly requested.
//!
//! Run the selected production contract manually with `GRIFFR_LIVE_SMOKE_GAME`,
//! `GRIFFR_LIVE_SMOKE_REGION`, `GRIFFR_LIVE_SMOKE_CHANNEL`, and
//! `GRIFFR_LIVE_SMOKE_SUB_CHANNEL` set, then invoke:
//!
//! ```bash
//! cargo test -p griffr-hypergryph-api test_real_api_contract_target -- --ignored --nocapture
//! ```

use crate::client::{ApiClient, MediaResponse};
use crate::protocol::DEFAULT_LANGUAGE;
use crate::types::{GameFileEntry, GetLatestGameResponse};
use griffr_core::{ChannelPair, GameId, RegionId};

fn assert_non_empty(label: &str, value: &str) {
    assert!(!value.trim().is_empty(), "{label} should not be empty");
}

fn assert_latest_payload_shape(info: &GetLatestGameResponse) {
    assert_non_empty("version", &info.version);
    assert!(
        matches!(info.action, 0..=2),
        "action should be one of 0, 1, 2, got {}",
        info.action
    );

    if info.has_full_package() {
        let pkg = info
            .pkg
            .as_ref()
            .expect("has_full_package implies pkg must be present");
        assert!(!pkg.packs.is_empty(), "pkg.packs should not be empty");
        assert_non_empty("pkg.total_size", &pkg.total_size);
        assert_non_empty("pkg.file_path", &pkg.file_path);

        for pack in &pkg.packs {
            assert_non_empty("pkg.packs[].url", &pack.url);
            assert_non_empty("pkg.packs[].md5", &pack.md5);
            assert_non_empty("pkg.packs[].package_size", &pack.package_size);
        }
    }

    if info.has_patch_package() {
        let patch = info
            .patch
            .as_ref()
            .expect("has_patch_package implies patch must be present");

        assert_non_empty("patch.url", &patch.url);
        assert_non_empty("patch.md5", &patch.md5);
        assert_non_empty("patch.file_id", &patch.file_id);
        assert!(
            !patch.patches.is_empty(),
            "patch.patches should not be empty"
        );

        for part in &patch.patches {
            assert_non_empty("patch.patches[].url", &part.url);
            assert_non_empty("patch.patches[].md5", &part.md5);
            assert_non_empty("patch.patches[].package_size", &part.package_size);
        }
    }
}

fn expected_cdn_fragment(region: RegionId) -> &'static str {
    match region {
        RegionId::Cn => ".hycdn.cn",
        RegionId::Sg => ".hg-cdn.com",
        RegionId::Kr | RegionId::En | RegionId::Jp => ".yo-star.com",
    }
}

fn assert_game_files_entries(entries: &[GameFileEntry]) {
    assert!(
        !entries.is_empty(),
        "game_files should contain at least one manifest entry"
    );

    for entry in entries.iter().take(20) {
        assert_non_empty("game_files[].path", &entry.path);
        assert_non_empty("game_files[].md5", &entry.md5);
        assert!(
            entry.size > 0,
            "game_files entry size should be > 0 for {}",
            entry.path
        );
    }
}

fn assert_media_payload_shape(media: &MediaResponse) {
    let banners = media
        .banners
        .as_ref()
        .expect("media response should include banners payload");
    for banner in &banners.banners {
        assert_non_empty("banner.url", &banner.url);
        assert_non_empty("banner.md5", &banner.md5);
    }

    let announcements = media
        .announcements
        .as_ref()
        .expect("media response should include announcements payload");
    for tab in &announcements.tabs {
        assert_non_empty("announcement.tab_name", &tab.tab_name);
        for item in &tab.announcements {
            assert_non_empty("announcement.id", &item.id);
            assert_non_empty("announcement.content", &item.content);
        }
    }

    let background = media
        .background
        .as_ref()
        .expect("media response should include background payload");
    assert_non_empty("main_bg_image.url", &background.main_bg_image.url);
    assert_non_empty("main_bg_image.md5", &background.main_bg_image.md5);

    let sidebar = media
        .sidebar
        .as_ref()
        .expect("media response should include sidebar payload");
    for item in &sidebar.sidebars {
        assert_non_empty("sidebar.media", &item.media);
        if let Some(pic) = item.pic.as_ref() {
            assert_non_empty("sidebar.pic.url", &pic.url);
            assert_non_empty("sidebar.pic.md5", &pic.md5);
        }
    }
}

async fn assert_latest_for_channel(
    client: &ApiClient,
    game: GameId,
    region: RegionId,
    channel: ChannelPair,
) {
    let target = crate::resolve_api_target(
        &game,
        region,
        &channel,
        &crate::ApiTargetOverrides::default(),
    )
    .unwrap();
    let info = client
        .get_latest_game(&target, None)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed get_latest_game for game={:?} channel={:?}: {err}",
                game, channel
            )
        });

    assert_eq!(
        info.request_version, "",
        "latest query should use empty request_version for game={:?} channel={:?}",
        game, channel
    );
    assert_latest_payload_shape(&info);

    if let Some(pkg) = &info.pkg {
        if let Some(first_pack) = pkg.packs.first() {
            assert!(
                first_pack.url.contains(expected_cdn_fragment(region)),
                "pkg pack URL should use the expected CDN family for game={:?} channel={:?}, got {}",
                game,
                channel,
                first_pack.url
            );
        }
    }

    if let Some(patch) = &info.patch {
        if let Some(first_patch) = patch.patches.first() {
            assert!(
                first_patch.url.contains(expected_cdn_fragment(region)),
                "patch URL should use the expected CDN family for game={:?} channel={:?}, got {}",
                game,
                channel,
                first_patch.url
            );
        }
    }
}

async fn assert_media_for_channel(
    client: &ApiClient,
    game: GameId,
    region: RegionId,
    channel: ChannelPair,
    language: &str,
) {
    let target = crate::resolve_api_target(
        &game,
        region,
        &channel,
        &crate::ApiTargetOverrides::default(),
    )
    .unwrap();
    let media = client
        .get_media(&target, language)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed get_media for game={:?} channel={:?} language={}: {err}",
                game, channel, language
            )
        });

    assert_media_payload_shape(&media);

    let cdn = expected_cdn_fragment(region);

    if let Some(banners) = &media.banners {
        if let Some(first) = banners.banners.first() {
            assert!(
                first.url.contains(cdn),
                "banner URL should use the expected CDN family for game={:?} channel={:?}, got {}",
                game,
                channel,
                first.url
            );
        }
    }

    if let Some(background) = &media.background {
        assert!(
            background.main_bg_image.url.contains(cdn),
            "background URL should use the expected CDN family for game={:?} channel={:?}, got {}",
            game,
            channel,
            background.main_bg_image.url
        );
    }

    if let Some(sidebar) = &media.sidebar {
        if let Some(item) = sidebar.sidebars.first() {
            if !item.jump_url.trim().is_empty() {
                assert!(
                    item.jump_url.starts_with("https://") || item.jump_url.starts_with("http://"),
                    "sidebar jump URL should be a real URL for game={:?} channel={:?}, got {}",
                    game,
                    channel,
                    item.jump_url
                );
            }
        }
    }
}

async fn assert_game_files_for_channel(
    client: &ApiClient,
    game: GameId,
    region: RegionId,
    channel: ChannelPair,
) {
    let target = crate::resolve_api_target(
        &game,
        region,
        &channel,
        &crate::ApiTargetOverrides::default(),
    )
    .unwrap();
    let info = client
        .get_latest_game(&target, None)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed get_latest_game for game={:?} channel={:?} before game_files fetch: {err}",
                game, channel
            )
        });

    let pkg = info.pkg.as_ref().unwrap_or_else(|| {
        panic!(
            "expected full package payload for game={:?} channel={:?} when checking game_files",
            game, channel
        )
    });

    let expected_md5 = pkg.game_files_md5.as_deref().unwrap_or_else(|| {
        panic!(
            "expected game_files_md5 for game={:?} channel={:?}",
            game, channel
        )
    });

    let entries = client
        .fetch_game_files(&pkg.file_path, Some(expected_md5))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed fetch_game_files for game={:?} channel={:?} base_url={}: {err}",
                game, channel, pkg.file_path
            )
        });

    assert_game_files_entries(&entries);
}

fn required_live_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for live API contract tests"))
}

fn selected_live_target() -> (GameId, RegionId, ChannelPair) {
    let game = required_live_env("GRIFFR_LIVE_SMOKE_GAME")
        .parse()
        .expect("valid live smoke game id");
    let region = required_live_env("GRIFFR_LIVE_SMOKE_REGION")
        .parse()
        .expect("valid live smoke region id");
    let channel = std::env::var("GRIFFR_LIVE_SMOKE_CHANNEL").ok();
    let sub_channel = std::env::var("GRIFFR_LIVE_SMOKE_SUB_CHANNEL").ok();
    let channels = ChannelPair::parse(region, channel, sub_channel)
        .expect("live Hypergryph contract target requires valid channel metadata");
    (game, region, channels)
}

#[compio::test]
#[ignore = "Makes real network requests to the selected Hypergryph/Gryphline deployment"]
async fn test_real_api_contract_target() {
    let client = ApiClient::new().expect("Failed to create API client");
    let (game, region, channels) = selected_live_target();
    let language = if region == RegionId::Sg {
        "en-us"
    } else {
        DEFAULT_LANGUAGE
    };

    assert_latest_for_channel(&client, game.clone(), region, channels.clone()).await;
    assert_media_for_channel(&client, game.clone(), region, channels.clone(), language).await;
    assert_game_files_for_channel(&client, game, region, channels).await;
}

mod known_versions;

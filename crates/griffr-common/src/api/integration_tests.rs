//! Integration tests against real Hypergryph API channels
//!
//! These tests make actual network requests and are marked with `#[ignore]`
//! so they only run when explicitly requested.
//!
//! Run them manually with:
//!
//! ```bash
//! cargo test -p griffr-common api::integration_tests -- --ignored --nocapture
//! ```

use crate::api::client::{ApiClient, MediaResponse};
use crate::api::protocol::DEFAULT_LANGUAGE;
use crate::api::types::{GameFileEntry, GetLatestGameResponse};
use crate::config::{ChannelPair, GameId, RegionId};

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
    let target = crate::config::resolve_api_target(
        &game,
        region,
        &channel,
        &crate::config::ApiTargetOverrides::default(),
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
    let target = crate::config::resolve_api_target(
        &game,
        region,
        &channel,
        &crate::config::ApiTargetOverrides::default(),
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
    let target = crate::config::resolve_api_target(
        &game,
        region,
        &channel,
        &crate::config::ApiTargetOverrides::default(),
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

#[compio::test]
#[ignore = "Makes real network request"]
async fn test_real_api_latest_matrix() {
    let client = ApiClient::new().expect("Failed to create API client");

    let matrix = [
        (
            GameId::ARKNIGHTS,
            RegionId::Cn,
            ChannelPair::from_api("1", None::<String>).unwrap(),
        ),
        (
            GameId::ARKNIGHTS,
            RegionId::Cn,
            ChannelPair::from_api("2", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Cn,
            ChannelPair::from_api("1", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Cn,
            ChannelPair::from_api("2", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", Some("801")).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", Some("802")).unwrap(),
        ),
    ];
    assert_eq!(
        matrix.len(),
        7,
        "latest matrix should cover all known CN/SG combinations"
    );

    for (game, region, channel) in matrix {
        assert_latest_for_channel(&client, game, region, channel).await;
    }
}

#[compio::test]
#[ignore = "Makes real network request"]
async fn test_real_api_media_matrix() {
    let client = ApiClient::new().expect("Failed to create API client");

    let matrix = [
        (
            GameId::ARKNIGHTS,
            RegionId::Cn,
            ChannelPair::from_api("1", None::<String>).unwrap(),
            DEFAULT_LANGUAGE,
        ),
        (
            GameId::ARKNIGHTS,
            RegionId::Cn,
            ChannelPair::from_api("2", None::<String>).unwrap(),
            DEFAULT_LANGUAGE,
        ),
        (
            GameId::ENDFIELD,
            RegionId::Cn,
            ChannelPair::from_api("1", None::<String>).unwrap(),
            DEFAULT_LANGUAGE,
        ),
        (
            GameId::ENDFIELD,
            RegionId::Cn,
            ChannelPair::from_api("2", None::<String>).unwrap(),
            DEFAULT_LANGUAGE,
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", None::<String>).unwrap(),
            "en-us",
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", Some("801")).unwrap(),
            "en-us",
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", Some("802")).unwrap(),
            "en-us",
        ),
    ];
    assert_eq!(
        matrix.len(),
        7,
        "media matrix should cover all known CN/SG combinations"
    );

    for (game, region, channel, language) in matrix {
        assert_media_for_channel(&client, game, region, channel, language).await;
    }
}

#[compio::test]
#[ignore = "Makes real network request"]
async fn test_real_api_game_files_matrix() {
    let client = ApiClient::new().expect("Failed to create API client");

    let matrix = [
        (
            GameId::ARKNIGHTS,
            RegionId::Cn,
            ChannelPair::from_api("1", None::<String>).unwrap(),
        ),
        (
            GameId::ARKNIGHTS,
            RegionId::Cn,
            ChannelPair::from_api("2", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Cn,
            ChannelPair::from_api("1", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Cn,
            ChannelPair::from_api("2", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", None::<String>).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", Some("801")).unwrap(),
        ),
        (
            GameId::ENDFIELD,
            RegionId::Sg,
            ChannelPair::from_api("6", Some("802")).unwrap(),
        ),
    ];
    assert_eq!(
        matrix.len(),
        7,
        "game_files matrix should cover all known CN/SG combinations"
    );

    for (game, region, channel) in matrix {
        assert_game_files_for_channel(&client, game, region, channel).await;
    }
}

mod known_versions;

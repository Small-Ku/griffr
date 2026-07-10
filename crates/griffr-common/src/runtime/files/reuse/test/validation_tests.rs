use super::super::*;
use crate::config::{ChannelId, GameId};
use crate::runtime::files::reuse::plan;
use std::path::PathBuf;
#[test]
fn test_reuse_plan_with_mixed_files() {
    let plan = ReusePlan {
        source_channels: vec![SourceChannel {
            channel_id: ChannelId::CN_OFFICIAL,
            version: "2.0.0".to_string(),
            install_path: PathBuf::from("/games/source"),
            file_count: 100,
        }],
        reusable_files: (0..80)
            .map(|i| ReusableFile {
                path: format!("assets/file_{:03}.bin", i),
                md5: format!("md5_{:03}", i),
                size: 1024 * 1024,
                source_channel_id: ChannelId::CN_OFFICIAL,
                source_path: PathBuf::from("/games/source"),
            })
            .collect(),
        download_files: (80..100)
            .map(|i| DownloadFile {
                path: format!("assets/file_{:03}.bin", i),
                md5: format!("new_md5_{:03}", i),
                size: 1024 * 1024,
            })
            .collect(),
        reusable_size: 80 * 1024 * 1024,
        download_size: 20 * 1024 * 1024,
        requires_copy_fallback: false,
    };

    assert_eq!(plan.reusable_files.len(), 80);
    assert_eq!(plan.download_files.len(), 20);
    assert_eq!(plan.reusable_size, 80 * 1024 * 1024);
    assert_eq!(plan.download_size, 20 * 1024 * 1024);
    let reuse_percentage = plan.reusable_files.len() as f64
        / (plan.reusable_files.len() + plan.download_files.len()) as f64
        * 100.0;
    assert!((reuse_percentage - 80.0).abs() < 0.1);
}

#[test]
fn test_game_id_channel_id_variants() {
    let games = [GameId::ARKNIGHTS, GameId::ENDFIELD];
    let channels = [
        ChannelId::CN_OFFICIAL,
        ChannelId::CN_BILIBILI,
        ChannelId::GLOBAL_OFFICIAL,
        ChannelId::GLOBAL_EPIC,
        ChannelId::GLOBAL_GOOGLEPLAY,
    ];
    for _game in &games {
        for _channel in &channels {}
    }
}

#[test]
fn test_is_launcher_metadata_path_matches_expected_names() {
    assert!(plan::is_launcher_metadata_path("config.ini"));
    assert!(plan::is_launcher_metadata_path("game_files"));
    assert!(plan::is_launcher_metadata_path("package_files"));
    assert!(plan::is_launcher_metadata_path("CONFIG.INI"));
    assert!(plan::is_launcher_metadata_path("Package_Files"));
    assert!(!plan::is_launcher_metadata_path("Endfield_Data/config.ini"));
    assert!(!plan::is_launcher_metadata_path("SomeGame/game_files.bin"));
}

#[test]
fn test_derive_files_base_url_from_game_files_suffix() {
    let url = "https://cdn.example.com/path/files/game_files";
    let base = plan::derive_files_base_url(url).unwrap();
    assert_eq!(base, "https://cdn.example.com/path/files");
}

#[test]
fn test_derive_files_base_url_from_files_suffix() {
    let url = "https://cdn.example.com/path/files";
    let base = plan::derive_files_base_url(url).unwrap();
    assert_eq!(base, "https://cdn.example.com/path/files");
}

#[test]
fn test_derive_files_base_url_rejects_unknown_shape() {
    let url = "https://cdn.example.com/path";
    let err = plan::derive_files_base_url(url).unwrap_err();
    assert!(err
        .to_string()
        .contains("Expected file_path to end with '/game_files' or '/files'"));
}

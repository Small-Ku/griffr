use super::super::*;
use crate::config::{ChannelPair, GameId};
use crate::runtime::files::reuse::plan;
use std::path::PathBuf;
#[test]
fn test_reuse_plan_with_mixed_files() {
    let plan = ReusePlan {
        source_channels: vec![SourceChannel {
            channel_id: ChannelPair::from_api("1", None::<String>).unwrap(),
            version: "2.0.0".to_string(),
            install_path: PathBuf::from("/games/source"),
            file_count: 100,
        }],
        reusable_files: (0..80)
            .map(|i| ReusableFile {
                path: format!("assets/file_{:03}.bin", i),
                md5: format!("md5_{:03}", i),
                size: 1024 * 1024,
                source_channel_id: ChannelPair::from_api("1", None::<String>).unwrap(),
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
        ChannelPair::from_api("1", None::<String>).unwrap(),
        ChannelPair::from_api("2", None::<String>).unwrap(),
        ChannelPair::from_api("6", None::<String>).unwrap(),
        ChannelPair::from_api("6", Some("801")).unwrap(),
        ChannelPair::from_api("6", Some("802")).unwrap(),
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

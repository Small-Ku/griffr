use std::path::{Path, PathBuf};

use super::*;
use crate::config::{ChannelId, GameConfig, GameId, KnownTargets};

#[test]
fn test_game_manager() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::CN_OFFICIAL).unwrap(),
    );

    assert!(!manager.is_installed());
    assert!(manager.install_path().is_none());

    manager.set_install_path("C:\\Games\\Endfield");
    assert!(manager.is_installed());
    assert_eq!(
        manager.install_path(),
        Some(Path::new("C:\\Games\\Endfield"))
    );

    assert_eq!(manager.active_channel(), ChannelId::CN_OFFICIAL);

    manager.set_active_channel(ChannelId::CN_BILIBILI);
    assert_eq!(manager.active_channel(), ChannelId::CN_BILIBILI);
}

#[test]
fn test_game_manager_arknights() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ARKNIGHTS,
        config,
        KnownTargets::resolve(&GameId::ARKNIGHTS, &ChannelId::CN_OFFICIAL).unwrap(),
    );

    manager.set_install_path("C:\\Games\\Arknights");
    assert_eq!(manager.game_id(), GameId::ARKNIGHTS);

    // Check exe path
    let exe_path = manager.game_exe_path();
    assert!(exe_path.is_some());
    assert!(exe_path
        .unwrap()
        .to_string_lossy()
        .contains("Arknights.exe"));
}

#[test]
fn test_game_manager_endfield() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::CN_OFFICIAL).unwrap(),
    );

    manager.set_install_path("C:\\Games\\Endfield");

    // Check exe path
    let exe_path = manager.game_exe_path();
    assert!(exe_path.is_some());
    assert!(exe_path.unwrap().to_string_lossy().contains("Endfield.exe"));

    // Check config.ini path
    let ini_path = manager.config_ini_path();
    assert!(ini_path.is_some());
    assert!(ini_path.unwrap().to_string_lossy().contains("config.ini"));
}

#[test]
fn test_channel_installation() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::CN_OFFICIAL).unwrap(),
    );

    manager.set_install_path("C:\\Games\\Endfield");
    manager.mark_channel_installed(ChannelId::CN_OFFICIAL, "1.1.9");

    assert!(manager.is_active_channel_installed());
    assert_eq!(manager.current_version(), Some("1.1.9"));

    // Check channel config
    let channel_config = manager.channel_config(ChannelId::CN_OFFICIAL);
    assert!(channel_config.is_some());
    let channel_config = channel_config.unwrap();
    assert!(channel_config.installed);
    assert_eq!(channel_config.version, Some("1.1.9".to_string()));
    assert!(channel_config.last_update.is_some());
}

#[test]
fn test_multiple_channels() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::CN_OFFICIAL).unwrap(),
    );
    manager.set_install_path("C:\\Games\\Endfield");

    // Install CN Official
    manager.mark_channel_installed(ChannelId::CN_OFFICIAL, "1.1.9");

    // Switch to Bilibili and install
    manager.set_active_channel(ChannelId::CN_BILIBILI);
    manager.mark_channel_installed(ChannelId::CN_BILIBILI, "1.1.9");

    // Both should be marked installed
    assert!(
        manager
            .channel_config(ChannelId::CN_OFFICIAL)
            .unwrap()
            .installed
    );
    assert!(
        manager
            .channel_config(ChannelId::CN_BILIBILI)
            .unwrap()
            .installed
    );

    // Active version should be Bilibili's
    assert_eq!(manager.current_version(), Some("1.1.9"));
}

#[test]
fn test_version_tracking() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::CN_OFFICIAL).unwrap(),
    );

    manager.set_version("1.0.0");
    assert_eq!(manager.current_version(), Some("1.0.0"));

    manager.set_version("2.0.0");
    assert_eq!(manager.current_version(), Some("2.0.0"));
}

#[test]
fn test_into_config() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::CN_OFFICIAL).unwrap(),
    );

    manager.set_install_path("C:\\Games\\Endfield");
    manager.set_version("1.1.9");
    manager.set_active_channel(ChannelId::CN_BILIBILI);

    let config = manager.into_config();
    assert_eq!(config.install_path, None);
    assert_eq!(config.version, Some("1.1.9".to_string()));
    assert_eq!(config.active_channel, ChannelId::CN_BILIBILI);
    assert_eq!(
        config.channel_install_path(ChannelId::CN_OFFICIAL),
        Some(PathBuf::from("C:\\Games\\Endfield"))
    );
}

#[test]
fn test_channel_config_mut() {
    let config = GameConfig::default();
    let mut manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::CN_OFFICIAL).unwrap(),
    );

    // Get or create channel config
    let channel_config = manager.channel_config_mut(ChannelId::CN_OFFICIAL);
    channel_config.installed = true;
    channel_config.version = Some("1.0.0".to_string());

    assert!(
        manager
            .channel_config(ChannelId::CN_OFFICIAL)
            .unwrap()
            .installed
    );
}

#[compio::test]
async fn test_write_config_ini_uses_launcher_format() {
    let temp = tempfile::tempdir().unwrap();
    let exe_path = temp.path().join("Endfield.exe");
    std::fs::write(&exe_path, b"endfield exe bytes").unwrap();

    let mut config = GameConfig {
        install_path: Some(temp.path().to_path_buf()),
        active_channel: ChannelId::GLOBAL_OFFICIAL,
        version: Some("1.2.4".to_string()),
        last_update: None,
        channels: Default::default(),
    };
    let channel = config
        .channels
        .entry(ChannelId::GLOBAL_OFFICIAL)
        .or_default();
    channel.installed = true;
    channel.install_path = Some(temp.path().to_path_buf());
    channel.version = Some("1.2.4".to_string());

    let manager = GameManager::new(
        GameId::ENDFIELD,
        config,
        KnownTargets::resolve(&GameId::ENDFIELD, &ChannelId::GLOBAL_OFFICIAL).unwrap(),
    );
    manager.write_config_ini().await.unwrap();

    let encrypted = std::fs::read(temp.path().join("config.ini")).unwrap();
    let decrypted = crate::api::crypto::decrypt_game_files(&encrypted).unwrap();

    assert!(decrypted.starts_with("[Game]\n"));
    assert!(decrypted.contains("version=1.2.4\n"));
    assert!(decrypted.contains("entry=Endfield.exe\n"));
    assert!(decrypted.contains("appcode=YDUTE5gscDZ229CW\n"));
    assert!(decrypted.contains("region=sg\n"));
    assert!(decrypted.contains("channel=6\n"));
    assert!(decrypted.contains("sub_channel=6\n"));
    assert!(decrypted.contains(
        "uninstall_params=\"{\\\"uninstall_path\\\": \\\"AntiCheatExpert/ACE-Setup64.exe\\\", \\\"uninstall_params\\\": \\\"-q\\\"}\"\n"
    ));
    assert!(decrypted.contains("entry_md5="));
}

#[test]
fn test_derive_files_base_url_handles_game_files_suffix() {
    let url = "https://cdn.example.com/path/files/game_files";
    assert_eq!(
        GameManager::derive_files_base_url(url),
        "https://cdn.example.com/path/files"
    );
}

#[test]
fn test_derive_files_base_url_handles_files_url() {
    let url = "https://cdn.example.com/path/files";
    assert_eq!(
        GameManager::derive_files_base_url(url),
        "https://cdn.example.com/path/files"
    );
}

#[test]
fn test_derive_game_files_url_handles_both_shapes() {
    let files = "https://cdn.example.com/path/files";
    let game_files = "https://cdn.example.com/path/files/game_files";
    assert_eq!(
        GameManager::derive_game_files_url(files),
        "https://cdn.example.com/path/files/game_files"
    );
    assert_eq!(
        GameManager::derive_game_files_url(game_files),
        "https://cdn.example.com/path/files/game_files"
    );
}

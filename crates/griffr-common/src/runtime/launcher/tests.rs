use super::*;
use std::path::PathBuf;

#[test]
fn test_main_exe_names() {
    use crate::config::{resolve_install_target, ChannelPair, InstallTargetOverrides, RegionId};
    let ark_target = resolve_install_target(
        &GameId::ARKNIGHTS,
        RegionId::Cn,
        &ChannelPair::from_api("1", None::<String>).unwrap(),
        &InstallTargetOverrides::default(),
    )
    .unwrap();
    let ark_launcher = Launcher::new(GameId::ARKNIGHTS, ark_target, PathBuf::from("/games/ark"));
    assert_eq!(
        ark_launcher.main_exe_name().to_string_lossy(),
        "Arknights.exe"
    );

    let end_target = resolve_install_target(
        &GameId::ENDFIELD,
        RegionId::Cn,
        &ChannelPair::from_api("1", None::<String>).unwrap(),
        &InstallTargetOverrides::default(),
    )
    .unwrap();
    let end_launcher = Launcher::new(GameId::ENDFIELD, end_target, PathBuf::from("/games/end"));
    assert_eq!(
        end_launcher.main_exe_name().to_string_lossy(),
        "Endfield.exe"
    );
}

#[test]
fn test_is_process_in_game_directory() {
    let game_dir = PathBuf::from("C:\\Games\\Endfield");

    let in_dir = PathBuf::from("C:\\Games\\Endfield\\Endfield.exe");
    assert!(is_process_in_game_directory(&in_dir, &game_dir));

    let in_subdir = PathBuf::from("C:\\Games\\Endfield\\bin\\game.exe");
    assert!(is_process_in_game_directory(&in_subdir, &game_dir));

    let outside = PathBuf::from("C:\\Windows\\notepad.exe");
    assert!(!is_process_in_game_directory(&outside, &game_dir));
}

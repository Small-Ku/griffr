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

#[cfg(windows)]
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

#[cfg(target_os = "linux")]
fn endfield_launcher(install_path: &std::path::Path) -> Launcher {
    use crate::config::{resolve_install_target, ChannelPair, InstallTargetOverrides, RegionId};

    let target = resolve_install_target(
        &GameId::ENDFIELD,
        RegionId::Cn,
        &ChannelPair::from_api("1", None::<String>).unwrap(),
        &InstallTargetOverrides::default(),
    )
    .unwrap();
    Launcher::new(GameId::ENDFIELD, target, install_path)
}

#[cfg(target_os = "linux")]
#[compio::test]
async fn wine_runner_launches_detects_and_stops_the_game() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir().unwrap();
    let install_path = temp.path().join("game");
    let prefix = temp.path().join("prefix");
    let runner = temp.path().join("fake-wine");
    let exe_path = install_path.join("Endfield.exe");
    let invocation_path = temp.path().join("invocation.txt");
    let cwd_path = temp.path().join("cwd.txt");
    let prefix_path = temp.path().join("prefix.txt");
    let unrelated_path = temp.path().join("other");
    let unrelated_exe = unrelated_path.join("Endfield.exe");

    fs::create_dir_all(&install_path).unwrap();
    fs::create_dir_all(&prefix).unwrap();
    fs::create_dir_all(&unrelated_path).unwrap();
    fs::copy("/bin/sleep", &exe_path).unwrap();
    fs::copy("/bin/sleep", &unrelated_exe).unwrap();
    let mut exe_permissions = fs::metadata(&exe_path).unwrap().permissions();
    exe_permissions.set_mode(0o755);
    fs::set_permissions(&exe_path, exe_permissions).unwrap();
    let mut unrelated_permissions = fs::metadata(&unrelated_exe).unwrap().permissions();
    unrelated_permissions.set_mode(0o755);
    fs::set_permissions(&unrelated_exe, unrelated_permissions).unwrap();

    let mut unrelated_child = Command::new(&unrelated_exe)
        .arg("30")
        .current_dir(&unrelated_path)
        .spawn()
        .unwrap();

    fs::write(
        &runner,
        format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\npwd > '{}'\nprintf '%s' \"$WINEPREFIX\" > '{}'\nexec \"$1\" 30\n",
            invocation_path.display(),
            cwd_path.display(),
            prefix_path.display(),
        ),
    )
    .unwrap();
    let mut runner_permissions = fs::metadata(&runner).unwrap().permissions();
    runner_permissions.set_mode(0o755);
    fs::set_permissions(&runner, runner_permissions).unwrap();

    let launcher = endfield_launcher(&install_path).with_wine_config(WineConfig {
        runner,
        prefix: Some(prefix.clone()),
    });
    let mut child = launcher.launch().await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while !launcher.is_game_running() && Instant::now() < deadline {
        compio::time::sleep(Duration::from_millis(20)).await;
    }

    let processes = launcher.find_game_processes();
    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].pid, child.id());
    assert!(processes[0].is_main);
    assert!(!processes[0].is_child);
    assert_eq!(
        fs::read_to_string(&invocation_path).unwrap(),
        exe_path.display().to_string()
    );
    assert_eq!(
        fs::read_to_string(&cwd_path).unwrap().trim_end(),
        install_path.display().to_string()
    );
    assert_eq!(
        fs::read_to_string(&prefix_path).unwrap(),
        prefix.display().to_string()
    );

    launcher.stop_game().await.unwrap();
    assert!(!launcher.is_game_running());
    assert!(
        unrelated_child.try_wait().unwrap().is_none(),
        "stop_game killed a same-named process outside the selected install"
    );
    unrelated_child.kill().unwrap();
    unrelated_child.wait().unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "Wine shim child did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Runs a real Wine process against a generated 64-bit PE fixture.
///
/// Run explicitly on a Linux host with Wine, clang, and lld-link:
/// `cargo test -p griffr-common real_wine_launch_smoke -- --ignored --nocapture`
#[cfg(target_os = "linux")]
#[compio::test]
#[ignore = "requires Wine plus clang and lld-link on the host"]
async fn real_wine_launch_smoke() {
    use std::fs;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn run(command: &mut Command) {
        let display = format!("{command:?}");
        let status = command.status().unwrap_or_else(|error| {
            panic!("failed to start {display}: {error}");
        });
        assert!(status.success(), "command failed: {display}");
    }

    let wine_runner = std::env::var_os("GRIFFR_WINE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wine"));
    run(Command::new(&wine_runner).arg("--version"));

    let temp = tempfile::tempdir().unwrap();
    let install_path = temp.path().join("game");
    let prefix = temp.path().join("prefix");
    fs::create_dir_all(&install_path).unwrap();

    let source_path = temp.path().join("game.c");
    let object_path = temp.path().join("game.obj");
    let definition_path = temp.path().join("kernel32.def");
    fs::write(
        &source_path,
        "__declspec(dllimport) void __stdcall Sleep(unsigned long);\n\
         __declspec(dllimport) void __stdcall ExitProcess(unsigned int);\n\
         void mainCRTStartup(void) { Sleep(60000); ExitProcess(0); }\n",
    )
    .unwrap();
    fs::write(
        &definition_path,
        "LIBRARY KERNEL32.dll\nEXPORTS\nSleep\nExitProcess\n",
    )
    .unwrap();

    run(Command::new("clang")
        .arg("--target=x86_64-pc-windows-msvc")
        .arg("-c")
        .arg(&source_path)
        .arg("-o")
        .arg(&object_path));
    run(Command::new("lld-link")
        .current_dir(temp.path())
        .arg("/lib")
        .arg("/def:kernel32.def")
        .arg("/machine:x64")
        .arg("/out:kernel32.lib"));
    run(Command::new("lld-link")
        .current_dir(temp.path())
        .arg("game.obj")
        .arg("kernel32.lib")
        .arg("/entry:mainCRTStartup")
        .arg("/subsystem:console")
        .arg("/nodefaultlib")
        .arg("/out:game/Endfield.exe"));

    let launcher = endfield_launcher(&install_path).with_wine_config(WineConfig {
        runner: wine_runner,
        prefix: Some(prefix.clone()),
    });
    let mut child = launcher.launch().await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(45);
    while !launcher.is_game_running() && Instant::now() < deadline {
        compio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        launcher.is_game_running(),
        "generated PE did not become visible as a Wine game process"
    );

    launcher.stop_game().await.unwrap();
    assert!(!launcher.is_game_running());
    child.wait().unwrap();

    let wineserver = std::env::var_os("GRIFFR_WINESERVER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wineserver"));
    let _ = Command::new(wineserver)
        .env("WINEPREFIX", prefix)
        .arg("-k")
        .status();
}

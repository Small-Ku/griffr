use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use super::{GameProcess, Launcher, WineConfig};
use crate::error::{Error, Result};

const PROC_ROOT: &str = "/proc";

pub(super) fn find_game_processes(launcher: &Launcher) -> Vec<GameProcess> {
    let mut processes = Vec::new();
    let Ok(entries) = fs::read_dir(PROC_ROOT) else {
        return processes;
    };

    let main_stem = launcher
        .main_exe_name()
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("");

    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(process) = identify_game_process(pid, main_stem, launcher) {
            processes.push(process);
        }
    }

    processes.sort_unstable_by_key(|process| process.pid);
    processes
}

pub(super) fn signal_process(launcher: &Launcher, pid: u32, signal: i32) -> Result<()> {
    let main_stem = launcher
        .main_exe_name()
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    if identify_game_process(pid, main_stem, launcher).is_none() {
        return Ok(());
    }

    let pid = libc::pid_t::try_from(pid).map_err(|_| Error::Message {
        context: "Launcher/Process error: ",
        detail: format!("Process ID {pid} does not fit this platform"),
    })?;

    // SAFETY: libc::kill does not retain pointers and accepts this integer PID/signal pair.
    let result = unsafe { libc::kill(pid, signal) };
    if result == 0 {
        return Ok(());
    }

    let source = std::io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }

    Err(Error::Message {
        context: "Launcher/Process error: ",
        detail: format!("Failed to signal process {pid}: {source}"),
    })
}

fn identify_game_process(pid: u32, main_stem: &str, launcher: &Launcher) -> Option<GameProcess> {
    if process_is_zombie(pid) {
        return None;
    }

    let name = read_process_name(pid)?;
    let name_stem = Path::new(&name)
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    let is_main = name_stem.eq_ignore_ascii_case(main_stem);
    let is_child = name_stem.eq_ignore_ascii_case("PlatformProcess")
        || name_stem.eq_ignore_ascii_case("NeoViewer");
    if !is_main && !is_child {
        return None;
    }

    let exe_path = process_game_path(pid, &name, launcher)?;
    Some(GameProcess {
        pid,
        name,
        exe_path,
        is_main,
        is_child,
    })
}

fn read_process_name(pid: u32) -> Option<String> {
    let value = fs::read_to_string(format!("{PROC_ROOT}/{pid}/comm")).ok()?;
    let value = value.trim_end_matches(['\r', '\n']);
    (!value.is_empty()).then(|| value.to_owned())
}

fn process_is_zombie(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("{PROC_ROOT}/{pid}/stat")) else {
        return false;
    };
    let Some(command_end) = stat.rfind(')') else {
        return false;
    };
    stat.as_bytes().get(command_end + 2) == Some(&b'Z')
}

fn process_game_path(pid: u32, name: &str, launcher: &Launcher) -> Option<PathBuf> {
    let proc_root = PathBuf::from(format!("{PROC_ROOT}/{pid}"));
    let install_root = canonical_or_original(&launcher.install_path);
    let cwd = fs::read_link(proc_root.join("cwd")).ok();

    if let Some(cwd) = cwd.as_deref() {
        if path_is_within(cwd, &install_root) {
            if let Some(path) = command_line_game_path(pid, cwd, launcher) {
                return Some(path);
            }
            return Some(cwd.join(name));
        }
    }

    if let Ok(exe_path) = fs::read_link(proc_root.join("exe")) {
        if path_is_within(&exe_path, &install_root) {
            return Some(exe_path);
        }
    }

    command_line_game_path(
        pid,
        cwd.as_deref().unwrap_or_else(|| Path::new("/")),
        launcher,
    )
}

fn command_line_game_path(pid: u32, cwd: &Path, launcher: &Launcher) -> Option<PathBuf> {
    let bytes = fs::read(format!("{PROC_ROOT}/{pid}/cmdline")).ok()?;
    let process_prefix = read_process_wine_prefix(pid).or_else(|| {
        launcher
            .wine_config()
            .and_then(WineConfig::effective_prefix)
    });

    bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| OsString::from_vec(arg.to_vec()))
        .filter_map(|arg| command_argument_path(&arg, cwd, process_prefix.as_deref()))
        .find(|path| path_is_within(path, &launcher.install_path))
}

fn command_argument_path(arg: &OsStr, cwd: &Path, wine_prefix: Option<&Path>) -> Option<PathBuf> {
    let unix_path = Path::new(arg);
    if unix_path.is_absolute() {
        return Some(unix_path.to_path_buf());
    }

    if let Some(path) = arg
        .to_str()
        .and_then(|value| windows_path_to_unix(value, wine_prefix))
    {
        return Some(path);
    }

    if unix_path.components().count() > 1 {
        return Some(cwd.join(unix_path));
    }
    None
}

fn read_process_wine_prefix(pid: u32) -> Option<PathBuf> {
    let bytes = fs::read(format!("{PROC_ROOT}/{pid}/environ")).ok()?;
    bytes
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"WINEPREFIX="))
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(OsString::from_vec(value.to_vec())))
}

pub(super) fn windows_path_to_unix(value: &str, wine_prefix: Option<&Path>) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    if bytes[2] != b'\\' && bytes[2] != b'/' {
        return None;
    }

    let drive = (bytes[0] as char).to_ascii_lowercase();
    let drive_root = match wine_prefix {
        Some(prefix) => {
            let dos_device = prefix.join("dosdevices").join(format!("{drive}:"));
            match fs::canonicalize(&dos_device) {
                Ok(path) => path,
                Err(source) if source.kind() == ErrorKind::NotFound && drive == 'c' => {
                    prefix.join("drive_c")
                }
                Err(source) if source.kind() == ErrorKind::NotFound && drive == 'z' => {
                    PathBuf::from("/")
                }
                Err(_) => return None,
            }
        }
        None if drive == 'z' => PathBuf::from("/"),
        None => return None,
    };

    let mut path = drive_root;
    for component in value[3..].split(['\\', '/']) {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            path.pop();
        } else {
            path.push(component);
        }
    }
    Some(path)
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    canonical_or_original(path).starts_with(canonical_or_original(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn maps_wine_drive_paths_through_dosdevices() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("prefix");
        let drive_c = prefix.join("drive_c");
        fs::create_dir_all(prefix.join("dosdevices")).unwrap();
        fs::create_dir_all(&drive_c).unwrap();
        symlink("../drive_c", prefix.join("dosdevices/c:")).unwrap();
        symlink("/", prefix.join("dosdevices/z:")).unwrap();

        assert_eq!(
            windows_path_to_unix("C:\\Games\\Endfield\\Endfield.exe", Some(&prefix)),
            Some(drive_c.join("Games/Endfield/Endfield.exe"))
        );
        assert_eq!(
            windows_path_to_unix("C:/Games/Endfield/Endfield.exe", Some(&prefix)),
            Some(drive_c.join("Games/Endfield/Endfield.exe"))
        );
        assert_eq!(
            windows_path_to_unix("Z:\\tmp\\Endfield.exe", Some(&prefix)),
            Some(PathBuf::from("/tmp/Endfield.exe"))
        );
    }

    #[test]
    fn rejects_relative_and_unc_windows_paths() {
        assert_eq!(windows_path_to_unix("Endfield.exe", None), None);
        assert_eq!(
            windows_path_to_unix("\\\\server\\share\\game.exe", None),
            None
        );
    }

    #[test]
    fn path_containment_is_component_aware() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("Endfield");
        let sibling = temp.path().join("Endfield-old");
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        assert!(path_is_within(&game.join("Endfield.exe"), &game));
        assert!(!path_is_within(&sibling.join("Endfield.exe"), &game));
    }
}

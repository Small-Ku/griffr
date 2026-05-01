//! Game launcher with process management and admin elevation
//!
//! Handles:
//! - Process detection (Arknights.exe, Endfield.exe, PlatformProcess.exe, NeoViewer.exe)
//! - Graceful kill sequence per TODO requirements
//! - Game launch

use std::io::ErrorKind;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::config::GameId;

/// Information about a running game process
#[derive(Debug, Clone)]
pub struct GameProcess {
    /// Process ID
    pub pid: u32,
    /// Process name
    pub name: String,
    /// Full path to executable
    pub exe_path: PathBuf,
    /// Whether this is the main game process
    pub is_main: bool,
    /// Whether this is a child process (PlatformProcess, NeoViewer)
    pub is_child: bool,
}

/// Game launcher with process management capabilities
#[derive(Debug)]
pub struct Launcher {
    game_id: GameId,
    install_path: PathBuf,
}

impl Launcher {
    /// Create a new launcher for the given game and installation path
    pub fn new(game_id: GameId, install_path: impl Into<PathBuf>) -> Self {
        Self {
            game_id,
            install_path: install_path.into(),
        }
    }

    /// Get the main game executable name for this game
    fn main_exe_name(&self) -> &'static str {
        match self.game_id {
            GameId::Arknights => "Arknights.exe",
            GameId::Endfield => "Endfield.exe",
        }
    }

    /// Get the full path to the main game executable
    pub fn game_exe_path(&self) -> PathBuf {
        self.install_path.join(self.main_exe_name())
    }

    /// Check if the game is currently running
    pub fn is_game_running(&self) -> bool {
        !self.find_game_processes().is_empty()
    }

    /// Find all game-related processes
    pub fn find_game_processes(&self) -> Vec<GameProcess> {
        #[cfg(windows)]
        {
            self.find_game_processes_windows()
        }
        #[cfg(not(windows))]
        {
            vec![]
        }
    }

    /// Find game processes on Windows
    #[cfg(windows)]
    fn find_game_processes_windows(&self) -> Vec<GameProcess> {
        use windows_sys::Win32::Foundation::{CloseHandle, TRUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };

        let mut processes = Vec::new();
        let main_exe = self.main_exe_name();
        let main_exe_stem = Path::new(main_exe)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot.is_null() {
                return processes;
            }

            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

            if Process32FirstW(snapshot, &mut entry) == TRUE {
                loop {
                    let process_name = String::from_utf16_lossy(&entry.szExeFile)
                        .trim_end_matches('\0')
                        .to_string();

                    let name_stem = Path::new(&process_name)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");

                    // Check if this is a process we care about
                    let is_main = name_stem.eq_ignore_ascii_case(main_exe_stem);
                    let is_child = name_stem.eq_ignore_ascii_case("PlatformProcess")
                        || name_stem.eq_ignore_ascii_case("NeoViewer");

                    if is_main || is_child {
                        let pid = entry.th32ProcessID;

                        // Try to get the full executable path
                        let exe_path = if let Ok(path) =
                            get_process_exe_path(pid, &self.install_path, &process_name)
                        {
                            path
                        } else {
                            continue;
                        };

                        // Only include processes that can be proven to belong to this install.
                        if is_process_in_game_directory(&exe_path, &self.install_path) {
                            processes.push(GameProcess {
                                pid,
                                name: process_name,
                                exe_path,
                                is_main,
                                is_child,
                            });
                        }
                    }

                    if Process32NextW(snapshot, &mut entry) != TRUE {
                        break;
                    }
                }
            }

            CloseHandle(snapshot);
        }

        processes
    }

    /// Gracefully kill the game process following the TODO sequence:
    /// 1. Kill child processes (PlatformProcess, NeoViewer) first
    /// 2. Kill main game exe
    /// 3. Wait 1.5s for handle release
    /// 4. Force kill if still running
    pub async fn kill_game(&self) -> Result<()> {
        let processes = self.find_game_processes();

        if processes.is_empty() {
            info!("No game processes found to kill");
            return Ok(());
        }

        info!("Found {} game process(es) to terminate", processes.len());

        // First pass: Graceful termination
        // 1. Kill child processes first
        for proc in &processes {
            if proc.is_child {
                debug!(
                    "Requesting graceful termination of child process: {} (PID: {})",
                    proc.name, proc.pid
                );
                if let Err(e) = self.request_graceful_termination(proc.pid) {
                    warn!(
                        "Failed to gracefully terminate {} (PID: {}): {}",
                        proc.name, proc.pid, e
                    );
                }
            }
        }

        // 2. Kill main game exe
        for proc in &processes {
            if proc.is_main {
                debug!(
                    "Requesting graceful termination of main process: {} (PID: {})",
                    proc.name, proc.pid
                );
                if let Err(e) = self.request_graceful_termination(proc.pid) {
                    warn!(
                        "Failed to gracefully terminate {} (PID: {}): {}",
                        proc.name, proc.pid, e
                    );
                }
            }
        }

        // 3. Wait 1.5 seconds for handles to release
        debug!("Waiting 1.5s for processes to terminate gracefully...");
        thread::sleep(Duration::from_millis(1500));

        // 4. Second pass: Force kill any remaining processes
        let remaining = self.find_game_processes();
        if !remaining.is_empty() {
            info!(
                "{} process(es) still running, force killing...",
                remaining.len()
            );

            for proc in &remaining {
                warn!("Force killing {} (PID: {})", proc.name, proc.pid);
                if let Err(e) = self.force_kill_process(proc.pid) {
                    warn!(
                        "Failed to force kill {} (PID: {}): {}",
                        proc.name, proc.pid, e
                    );
                }
            }

            // Wait a bit more and verify
            thread::sleep(Duration::from_millis(500));
            let final_check = self.find_game_processes();
            if !final_check.is_empty() {
                return Err(anyhow::anyhow!(
                    "Failed to kill {} process(es): {:?}",
                    final_check.len(),
                    final_check.iter().map(|p| &p.name).collect::<Vec<_>>()
                ));
            }
        }

        info!("Game terminated successfully");
        Ok(())
    }

    /// Request graceful termination of a process (WM_CLOSE on Windows)
    #[cfg(windows)]
    fn request_graceful_termination(&self, pid: u32) -> Result<()> {
        use windows_sys::Win32::Foundation::{LPARAM, TRUE, WPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
        };

        // First try to send WM_CLOSE to any windows owned by this process
        unsafe {
            struct EnumData {
                pid: u32,
                sent: bool,
            }

            let mut data = EnumData { pid, sent: false };

            unsafe extern "system" fn enum_callback(
                hwnd: *mut std::ffi::c_void,
                lparam: LPARAM,
            ) -> i32 {
                let data = &mut *(lparam as *mut EnumData);

                let mut window_pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut window_pid);

                if window_pid == data.pid {
                    // Send WM_CLOSE to this window
                    PostMessageW(hwnd, WM_CLOSE, 0 as WPARAM, 0 as LPARAM);
                    data.sent = true;
                }

                TRUE // Continue enumeration
            }

            EnumWindows(Some(enum_callback), &mut data as *mut _ as LPARAM);

            if data.sent {
                return Ok(());
            }
        }

        // If no windows found or WM_CLOSE failed, fall back to terminating the process
        self.force_kill_process(pid)
    }

    #[cfg(not(windows))]
    fn request_graceful_termination(&self, _pid: u32) -> Result<()> {
        Ok(())
    }

    /// Force kill a process
    #[cfg(windows)]
    fn force_kill_process(&self, pid: u32) -> Result<()> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };

        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                return Err(anyhow::anyhow!(
                    "Failed to open process {} for termination",
                    pid
                ));
            }

            let result = TerminateProcess(handle, 1);
            CloseHandle(handle);

            if result == 0 {
                return Err(anyhow::anyhow!("TerminateProcess failed for PID {}", pid));
            }

            Ok(())
        }
    }

    #[cfg(not(windows))]
    fn force_kill_process(&self, _pid: u32) -> Result<()> {
        Ok(())
    }

    /// Launch the game
    pub async fn launch(&self) -> Result<Child> {
        let exe_path = self.game_exe_path();

        match compio::fs::metadata(&exe_path).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(anyhow::anyhow!(
                    "Game executable not found: {}",
                    exe_path.display()
                ));
            }
            Err(err) => {
                return Err(err).map_err(anyhow::Error::from).with_context(|| {
                    format!("Failed to stat game executable {}", exe_path.display())
                });
            }
        }

        let working_dir = exe_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.install_path.clone());

        info!("Launching {:?} from {:?}...", self.game_id, exe_path);

        #[cfg(windows)]
        {
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;

            let mut cmd = Command::new(&exe_path);
            cmd.current_dir(&working_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NEW_CONSOLE | CREATE_UNICODE_ENVIRONMENT);

            let child = cmd
                .spawn()
                .with_context(|| format!("Failed to launch game from {:?}", exe_path))?;

            info!("Game launched with PID: {:?}", child.id());
            Ok(child)
        }

        #[cfg(not(windows))]
        {
            let mut cmd = Command::new(&exe_path);
            cmd.current_dir(&working_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            let child = cmd
                .spawn()
                .with_context(|| format!("Failed to launch game from {:?}", exe_path))?;

            info!("Game launched with PID: {:?}", child.id());
            Ok(child)
        }
    }
}

/// Check if a process executable is within the game installation directory
fn is_process_in_game_directory(exe_path: &Path, game_dir: &Path) -> bool {
    exe_path.ancestors().any(|ancestor| ancestor == game_dir)
}

/// Try to get the full executable path for a process
#[cfg(windows)]
fn get_process_exe_path(pid: u32, _game_dir: &Path, _fallback_name: &str) -> Result<PathBuf> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::K32GetProcessImageFileNameW;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return Err(anyhow::anyhow!("Failed to open process"));
        }

        let mut buffer = [0u16; 260]; // MAX_PATH
        let result = K32GetProcessImageFileNameW(handle, buffer.as_mut_ptr(), 260);
        CloseHandle(handle);

        if result == 0 {
            return Err(anyhow::anyhow!("Failed to get process image name"));
        }

        let path = String::from_utf16_lossy(&buffer[..result as usize]);

        // Convert device path to DOS path if needed
        // \Device\HarddiskVolume3\... -> C:\...
        let path = if path.starts_with("\\Device\\") {
            convert_device_path_to_dos_path(&path).unwrap_or_else(|| PathBuf::from(&path))
        } else {
            PathBuf::from(&path)
        };

        Ok(path)
    }
}

#[cfg(windows)]
fn convert_device_path_to_dos_path(device_path: &str) -> Option<PathBuf> {
    // Map common device paths to drive letters
    use windows_sys::Win32::Storage::FileSystem::{GetLogicalDriveStringsW, QueryDosDeviceW};

    unsafe {
        let mut drives = [0u16; 256];
        let len = GetLogicalDriveStringsW(256, drives.as_mut_ptr());

        if len == 0 {
            return None;
        }

        let drives_str = String::from_utf16_lossy(&drives[..len as usize]);

        for drive in drives_str.split('\0').filter(|s| !s.is_empty()) {
            let drive_letter = drive.trim_end_matches('\\');
            let drive_prefix: String = drive_letter.chars().take(2).collect(); // e.g., "C:"

            let mut target = [0u16; 256];
            let device_name: Vec<u16> = drive_prefix.encode_utf16().chain(Some(0)).collect();

            if QueryDosDeviceW(device_name.as_ptr(), target.as_mut_ptr(), 256) != 0 {
                let target_str = String::from_utf16_lossy(&target);
                let target_str = target_str.trim_end_matches('\0');

                if let Some(remainder) = device_path.strip_prefix(target_str) {
                    let result = format!("{}{}", drive_letter, remainder);
                    return Some(PathBuf::from(result));
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_main_exe_names() {
        let ark_launcher = Launcher::new(GameId::Arknights, PathBuf::from("/games/ark"));
        assert_eq!(ark_launcher.main_exe_name(), "Arknights.exe");

        let end_launcher = Launcher::new(GameId::Endfield, PathBuf::from("/games/end"));
        assert_eq!(end_launcher.main_exe_name(), "Endfield.exe");
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
}

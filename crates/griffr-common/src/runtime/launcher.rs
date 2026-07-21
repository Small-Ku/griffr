//! Start and stop game processes, and request administrator rights
//!
//! Handles:
//! - Process detection (Arknights.exe, Endfield.exe, PlatformProcess.exe, NeoViewer.exe)
//! - Ordered process stop sequence
//! - Game launch

use std::io::ErrorKind;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};
use tracing::{debug, info, warn};

use crate::config::{GameId, InstallTarget};

/// Data for a running game process
#[derive(Debug, Clone)]
pub struct GameProcess {
    /// Process ID
    pub pid: u32,
    /// Process name
    pub name: String,
    /// Full path to executable
    pub exe_path: PathBuf,
    /// True for the main game process
    pub is_main: bool,
    /// True for a child process (PlatformProcess or NeoViewer)
    pub is_child: bool,
}

/// Start and stop game processes
#[derive(Debug)]
pub struct Launcher {
    game_id: GameId,
    target: InstallTarget,
    install_path: PathBuf,
}

impl Launcher {
    /// Create a launcher for a game, target, and install path
    pub fn new(game_id: GameId, target: InstallTarget, install_path: impl Into<PathBuf>) -> Self {
        Self {
            game_id,
            target,
            install_path: install_path.into(),
        }
    }

    /// Get the main game executable name
    fn main_exe_name(&self) -> &Path {
        &self.target.executable
    }

    /// Get the full path of the main game executable
    pub fn game_exe_path(&self) -> Result<PathBuf> {
        Ok(self.install_path.join(self.main_exe_name()))
    }

    /// Check if the game is running
    pub fn is_game_running(&self) -> bool {
        !self.find_game_processes().is_empty()
    }

    /// Find all processes for this game
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
        let main_exe_stem = main_exe.file_stem().and_then(|s| s.to_str()).unwrap_or("");

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

                    // Check for a game process.
                    let is_main = name_stem.eq_ignore_ascii_case(main_exe_stem);
                    let is_child = name_stem.eq_ignore_ascii_case("PlatformProcess")
                        || name_stem.eq_ignore_ascii_case("NeoViewer");

                    if is_main || is_child {
                        let pid = entry.th32ProcessID;

                        // Get the full executable path.
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

    /// Stop the game processes in this order:
    /// 1. Ask child processes to stop.
    /// 2. Ask the main process to stop.
    /// 3. Wait 1.5 seconds for handles to close.
    /// 4. Force-stop processes that are still running.
    pub async fn stop_game(&self) -> Result<()> {
        let processes = self.find_game_processes();

        if processes.is_empty() {
            info!("No game processes are running");
            return Ok(());
        }

        info!("Found {} game process(es) to stop", processes.len());

        // First pass: ask each process to stop.
        // Ask child processes to stop first.
        for proc in &processes {
            if proc.is_child {
                debug!(
                    "Requesting stop of child process: {} (PID: {})",
                    proc.name, proc.pid
                );
                if let Err(e) = self.request_process_stop(proc.pid) {
                    warn!("Failed to stop {} (PID: {}): {}", proc.name, proc.pid, e);
                }
            }
        }

        // Ask the main game process to stop.
        for proc in &processes {
            if proc.is_main {
                debug!(
                    "Requesting stop of main process: {} (PID: {})",
                    proc.name, proc.pid
                );
                if let Err(e) = self.request_process_stop(proc.pid) {
                    warn!("Failed to stop {} (PID: {}): {}", proc.name, proc.pid, e);
                }
            }
        }

        // Wait 1.5 seconds for handles to close.
        debug!("Waiting 1.5 seconds for processes to stop...");
        compio::time::sleep(Duration::from_millis(1500)).await;

        // Second pass: force-stop remaining processes.
        let remaining = self.find_game_processes();
        if !remaining.is_empty() {
            info!(
                "{} process(es) are still running; force-stop them...",
                remaining.len()
            );

            for proc in &remaining {
                warn!("Force-stopping {} (PID: {})", proc.name, proc.pid);
                if let Err(e) = self.force_stop_process(proc.pid) {
                    warn!(
                        "Failed to force-stop {} (PID: {}): {}",
                        proc.name, proc.pid, e
                    );
                }
            }

            // Wait and check again.
            compio::time::sleep(Duration::from_millis(500)).await;
            let final_check = self.find_game_processes();
            if !final_check.is_empty() {
                return Err(Error::Launcher(format!(
                    "Failed to stop {} process(es): {:?}",
                    final_check.len(),
                    final_check.iter().map(|p| &p.name).collect::<Vec<_>>()
                )));
            }
        }

        info!("Game stopped");
        Ok(())
    }

    /// Ask a process to stop with WM_CLOSE on Windows
    #[cfg(windows)]
    fn request_process_stop(&self, pid: u32) -> Result<()> {
        use windows_sys::Win32::Foundation::{LPARAM, TRUE, WPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
        };

        // Send WM_CLOSE to each window that belongs to this process.
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
                    // Send WM_CLOSE to this window.
                    PostMessageW(hwnd, WM_CLOSE, 0 as WPARAM, 0 as LPARAM);
                    data.sent = true;
                }

                TRUE // Continue the search.
            }

            EnumWindows(Some(enum_callback), &mut data as *mut _ as LPARAM);

            if data.sent {
                return Ok(());
            }
        }

        // Force-stop the process if it has no window or WM_CLOSE fails.
        self.force_stop_process(pid)
    }

    #[cfg(not(windows))]
    fn request_process_stop(&self, _pid: u32) -> Result<()> {
        Ok(())
    }

    /// Force-stop a process
    #[cfg(windows)]
    fn force_stop_process(&self, pid: u32) -> Result<()> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };

        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                return Err(Error::Launcher(format!(
                    "Failed to open process {} to stop it",
                    pid
                )));
            }

            let result = TerminateProcess(handle, 1);
            CloseHandle(handle);

            if result == 0 {
                return Err(Error::Launcher(format!(
                    "TerminateProcess failed for PID {}",
                    pid
                )));
            }

            Ok(())
        }
    }

    #[cfg(not(windows))]
    fn force_stop_process(&self, _pid: u32) -> Result<()> {
        Ok(())
    }

    /// Start the game
    pub async fn launch(&self) -> Result<Child> {
        let exe_path = self.game_exe_path()?;

        match compio::fs::metadata(&exe_path).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(Error::Launcher(format!(
                    "Game executable not found: {}",
                    exe_path.display()
                )));
            }
            Err(err) => {
                return Err(Error::StatFailed {
                    path: exe_path,
                    source: err,
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

            let child = cmd.spawn().map_err(|e| {
                Error::Launcher(format!("Failed to launch game from {:?}: {e}", exe_path))
            })?;

            info!("Game launched with PID: {:?}", child.id());
            Ok(child)
        }

        #[cfg(not(windows))]
        {
            let mut cmd = Command::new(&exe_path);
            cmd.current_dir(&working_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            let child = cmd.spawn().map_err(|e| {
                Error::Launcher(format!("Failed to launch game from {:?}: {e}", exe_path))
            })?;

            info!("Game launched with PID: {:?}", child.id());
            Ok(child)
        }
    }
}

/// Check if a process executable is within the game installation directory
fn is_process_in_game_directory(exe_path: &Path, game_dir: &Path) -> bool {
    exe_path.ancestors().any(|ancestor| ancestor == game_dir)
}

/// Get the full executable path. for a process
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
            return Err(Error::Launcher("Failed to open process".to_string()));
        }

        let mut buffer = [0u16; 260]; // MAX_PATH
        let result = K32GetProcessImageFileNameW(handle, buffer.as_mut_ptr(), 260);
        CloseHandle(handle);

        if result == 0 {
            return Err(Error::Launcher(
                "Failed to get process image name".to_string(),
            ));
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
mod tests;

//! Windows admin elevation utilities
//!
//! Provides functions to check for admin privileges and self-elevate
//! when running the launch command.

use anyhow::Result;
use tracing::{debug, info, warn};

/// Check if the current process is running with administrator privileges
#[cfg(windows)]
pub fn is_running_as_admin() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::GetTokenInformation;
    use windows_sys::Win32::Security::TokenElevation;
    use windows_sys::Win32::Security::TOKEN_ELEVATION;
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use windows_sys::Win32::System::Threading::OpenProcessToken;

    unsafe {
        let mut token = std::ptr::null_mut();
        let result = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token);
        if result == 0 {
            return false;
        }

        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;

        let result = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            size,
            &mut size,
        );

        CloseHandle(token);

        if result == 0 {
            return false;
        }

        elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_running_as_admin() -> bool {
    // On non-Windows platforms, assume we have sufficient permissions
    true
}

/// Restart the current executable with admin privileges
///
/// This function will:
/// 1. Get the current executable path
/// 2. Get the current command line arguments
/// 3. Use ShellExecute to relaunch with "runas" verb
/// 4. Exit the current process
///
/// Note: This function never returns on success - it exits the process
#[cfg(windows)]
pub fn restart_as_admin() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    // Get current executable path
    let exe_path = std::env::current_exe().expect("Failed to get current executable path");

    // Get current arguments (skip the first one which is the exe path)
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args_str = args.join(" ");

    // Convert to wide strings
    let exe_wide: Vec<u16> = exe_path.as_os_str().encode_wide().chain(Some(0)).collect();

    let args_wide: Vec<u16> = OsString::from(&args_str)
        .encode_wide()
        .chain(Some(0))
        .collect();

    let runas_wide: Vec<u16> = OsString::from("runas")
        .encode_wide()
        .chain(Some(0))
        .collect();

    unsafe {
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            runas_wide.as_ptr(),
            exe_wide.as_ptr(),
            args_wide.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );

        // ShellExecute returns a value > 32 on success
        let result_as_int = result as isize;
        if result_as_int <= 32 {
            let error = GetLastError();
            panic!(
                "Failed to elevate to admin. Error code: {}. \
                 This may happen if you clicked 'No' on the UAC prompt.",
                error
            );
        }

        // Exit the current (non-elevated) process
        std::process::exit(0);
    }
}

#[cfg(not(windows))]
pub fn restart_as_admin() {
    panic!("Admin elevation not supported on this platform");
}

/// Ensure the process is running as admin, or restart with elevation
///
/// If the process is already running as admin, this function returns Ok(()).
/// Otherwise, it attempts to restart with admin privileges and exits the current process.
///
/// Note: This function does not return if elevation is needed - the process exits instead
pub fn ensure_admin() -> Result<()> {
    if is_running_as_admin() {
        debug!("Already running as administrator");
        return Ok(());
    }

    info!("Requesting administrator privileges...");
    warn!("A UAC prompt will appear. Please click 'Yes' to continue.");

    restart_as_admin();

    // This is unreachable - restart_as_admin always exits
    #[allow(unreachable_code)]
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_running_as_admin_does_not_panic() {
        // Just ensure the function doesn't panic
        let _ = is_running_as_admin();
    }
}

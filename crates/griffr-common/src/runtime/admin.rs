//! Windows administrator elevation utilities.

use crate::error::{Error, Result};
use tracing::{debug, info, warn};

/// Check if the current process is running with administrator privileges.
#[cfg(windows)]
pub fn is_running_as_admin() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
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
        result != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_running_as_admin() -> bool {
    true
}

/// Quote one argument using the parsing rules consumed by `CommandLineToArgvW`.
///
/// The caller joins quoted arguments with one ASCII space. Backslashes are
/// doubled only where they precede a quote or the closing quote.
#[cfg(any(windows, test))]
fn quote_windows_argument_units(argument: &[u16]) -> Vec<u16> {
    const BACKSLASH: u16 = b'\\' as u16;
    const QUOTE: u16 = b'"' as u16;
    const SPACE: u16 = b' ' as u16;
    const TAB: u16 = b'\t' as u16;

    let needs_quotes = argument.is_empty()
        || argument
            .iter()
            .any(|value| matches!(*value, SPACE | TAB | QUOTE));
    if !needs_quotes {
        return argument.to_vec();
    }

    let mut quoted = Vec::with_capacity(argument.len() + 2);
    quoted.push(QUOTE);
    let mut backslashes = 0usize;
    for &value in argument {
        if value == BACKSLASH {
            backslashes += 1;
            continue;
        }
        if value == QUOTE {
            quoted.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
            quoted.push(QUOTE);
        } else {
            quoted.extend(std::iter::repeat_n(BACKSLASH, backslashes));
            quoted.push(value);
        }
        backslashes = 0;
    }
    quoted.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    quoted.push(QUOTE);
    quoted
}

#[cfg(windows)]
fn current_parameter_line() -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    let mut line = Vec::new();
    for argument in std::env::args_os().skip(1) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        let units = argument.as_os_str().encode_wide().collect::<Vec<_>>();
        line.extend(quote_windows_argument_units(&units));
    }
    line.push(0);
    line
}

/// Restart the current executable with administrator privileges.
///
/// On success this function terminates the current process. Failures are
/// returned to the caller instead of panicking.
#[cfg(windows)]
pub fn restart_as_admin() -> Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe_path = std::env::current_exe()?;
    let exe_wide = exe_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let args_wide = current_parameter_line();
    let runas_wide = OsStr::new("runas")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            runas_wide.as_ptr(),
            exe_wide.as_ptr(),
            args_wide.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    let shell_code = result as isize;
    if shell_code <= 32 {
        let os_error = unsafe { GetLastError() };
        return Err(Error::Message {
            context: "Failed to request administrator privileges: ",
            detail: format!(
                "ShellExecuteW returned {shell_code} (OS error {os_error}); the UAC prompt may have been cancelled"
            ),
        });
    }

    std::process::exit(0);
}

#[cfg(not(windows))]
pub fn restart_as_admin() -> Result<()> {
    Err(Error::Message {
        context: "Administrator elevation is unavailable: ",
        detail: "this operation is supported only on Windows".to_string(),
    })
}

/// Ensure the process is running as administrator, or restart it elevated.
pub fn ensure_admin() -> Result<()> {
    if is_running_as_admin() {
        debug!("Already running as administrator");
        return Ok(());
    }

    info!("Requesting administrator privileges...");
    warn!("A UAC prompt will appear. Approve it to continue.");
    restart_as_admin()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(argument: &str) -> String {
        String::from_utf16(&quote_windows_argument_units(
            &argument.encode_utf16().collect::<Vec<_>>(),
        ))
        .unwrap()
    }

    #[test]
    fn admin_probe_does_not_panic() {
        let _ = is_running_as_admin();
    }

    #[test]
    fn windows_arguments_quote_spaces_quotes_and_trailing_slashes() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(
            quote(r"C:\\Game Files\\config.ini"),
            r#""C:\\Game Files\\config.ini""#
        );
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote("path with slash\\"), "\"path with slash\\\\\"");
    }
}

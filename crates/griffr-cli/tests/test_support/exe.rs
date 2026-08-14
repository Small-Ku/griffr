use std::ffi::OsString;

pub fn griffr_exe() -> OsString {
    std::env::var_os("NEXTEST_BIN_EXE_griffr")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_griffr"))
        .unwrap_or_else(|| OsString::from(env!("CARGO_BIN_EXE_griffr")))
}

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod api;
pub mod config;
mod download;
pub mod error;
pub mod runtime;

/// Formats a byte slice as a lowercase hexadecimal string.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize] as char);
        s.push(HEX_CHARS[(b & 0xf) as usize] as char);
    }
    s
}

mod backend;
mod catalog;
mod channel;
mod error;
mod game;
mod region;
mod target;
mod url_path;

pub use backend::*;
pub use catalog::*;
pub use channel::*;
pub use error::{Error, Result};
pub use game::*;
pub use region::*;
pub use target::*;
pub use url_path::build_cdn_file_url;

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

use std::fmt;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Canonical content digest carried by manifests and task-pool integrity work.
///
/// Hypergryph/Gryphline manifests use lowercase hexadecimal MD5. The observed
/// YoStar Arknights EN launcher uses CRC-64/XZ and serializes it as an unsigned
/// decimal integer string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "algorithm", content = "value", rename_all = "snake_case")]
pub enum ContentHash {
    Md5(String),
    Crc64Xz(u64),
}

impl ContentHash {
    pub fn md5(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref().trim();
        if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!("invalid MD5 digest {value:?}"),
            });
        }
        Ok(Self::Md5(value.to_ascii_lowercase()))
    }

    pub fn crc64_xz_decimal(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref().trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!("invalid CRC64-XZ digest {value:?}"),
            });
        }
        let parsed = value.parse::<u64>().map_err(|_| Error::Message {
            context: "Integrity error: ",
            detail: format!("CRC64-XZ digest is outside u64 range: {value:?}"),
        })?;
        Ok(Self::Crc64Xz(parsed))
    }

    pub fn algorithm_name(&self) -> &'static str {
        match self {
            Self::Md5(_) => "md5",
            Self::Crc64Xz(_) => "crc64-xz",
        }
    }

    pub fn manifest_value(&self) -> String {
        match self {
            Self::Md5(value) => value.clone(),
            Self::Crc64Xz(value) => value.to_string(),
        }
    }

    pub(crate) fn hasher(&self) -> ContentHasher {
        match self {
            Self::Md5(_) => ContentHasher::Md5(Md5::new()),
            Self::Crc64Xz(_) => ContentHasher::Crc64Xz(Crc64Xz::new()),
        }
    }
}

impl From<&ContentHash> for ContentHash {
    fn from(value: &ContentHash) -> Self {
        value.clone()
    }
}

impl From<String> for ContentHash {
    fn from(value: String) -> Self {
        Self::Md5(value.to_ascii_lowercase())
    }
}

impl From<&str> for ContentHash {
    fn from(value: &str) -> Self {
        Self::Md5(value.to_ascii_lowercase())
    }
}

impl From<&String> for ContentHash {
    fn from(value: &String) -> Self {
        Self::Md5(value.to_ascii_lowercase())
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.manifest_value())
    }
}

#[derive(Clone)]
pub(crate) enum ContentHasher {
    Md5(Md5),
    Crc64Xz(Crc64Xz),
}

impl ContentHasher {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Md5(hasher) => Digest::update(hasher, bytes),
            Self::Crc64Xz(hasher) => hasher.update(bytes),
        }
    }

    pub(crate) fn finalize(self) -> ContentHash {
        match self {
            Self::Md5(hasher) => ContentHash::Md5(crate::to_hex(&Digest::finalize(hasher))),
            Self::Crc64Xz(hasher) => ContentHash::Crc64Xz(hasher.finalize()),
        }
    }
}

/// Reflected CRC-64/ECMA polynomial with the init/xorout used by CRC-64/XZ.
/// The inspected YoStar launcher embeds the same polynomial and produces the
/// standard CRC-64/XZ check value for `123456789`.
const CRC64_XZ_POLY: u64 = 0xc96c_5795_d787_0f42;

const fn crc64_xz_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut index = 0usize;
    while index < table.len() {
        let mut crc = index as u64;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ CRC64_XZ_POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

const CRC64_XZ_TABLE: [u64; 256] = crc64_xz_table();

#[derive(Debug, Clone)]
pub(crate) struct Crc64Xz {
    state: u64,
}

impl Crc64Xz {
    pub(crate) const fn new() -> Self {
        Self { state: u64::MAX }
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let mut crc = self.state;
        for &byte in bytes {
            let index = ((crc ^ u64::from(byte)) & 0xff) as usize;
            crc = CRC64_XZ_TABLE[index] ^ (crc >> 8);
        }
        self.state = crc;
    }

    pub(crate) const fn finalize(self) -> u64 {
        !self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc64_xz_matches_yostar_launcher_check_value() {
        let mut hasher = Crc64Xz::new();
        hasher.update(b"123456789");
        let digest = hasher.finalize();
        assert_eq!(digest, 0x995d_c9bb_df19_39fa);
        assert_eq!(digest.to_string(), "11051210869376104954");
    }

    #[test]
    fn content_hash_normalizes_md5_and_parses_decimal_crc64() {
        assert_eq!(
            ContentHash::md5("AABBCCDDEEFF00112233445566778899").unwrap(),
            ContentHash::Md5("aabbccddeeff00112233445566778899".to_string())
        );
        assert_eq!(
            ContentHash::crc64_xz_decimal("11051210869376104954").unwrap(),
            ContentHash::Crc64Xz(0x995d_c9bb_df19_39fa)
        );
    }
}

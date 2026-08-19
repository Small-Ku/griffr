use std::fmt;

use crc_fast::{CrcAlgorithm, Digest as CrcDigest};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Canonical content digest carried by manifests and task-pool integrity work.
///
/// Hypergryph/Gryphline manifests use lowercase hexadecimal MD5. The observed
/// YoStar Arknights KR/EN/JP launchers use CRC-64/XZ and serializes it as an unsigned
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
            Self::Crc64Xz(_) => {
                ContentHasher::Crc64Xz(Box::new(CrcDigest::new(CrcAlgorithm::Crc64Xz)))
            }
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
    // crc-fast keeps a comparatively large algorithm state. Box it so the
    // common MD5 hasher does not inherit that enum size on every stream.
    Crc64Xz(Box<CrcDigest>),
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
            Self::Md5(hasher) => ContentHash::Md5(griffr_core::to_hex(&Digest::finalize(hasher))),
            Self::Crc64Xz(hasher) => ContentHash::Crc64Xz((*hasher).finalize()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc64_xz_matches_yostar_launcher_check_value() {
        let mut hasher = ContentHash::Crc64Xz(0).hasher();
        hasher.update(b"123456789");
        let digest = match hasher.finalize() {
            ContentHash::Crc64Xz(value) => value,
            ContentHash::Md5(_) => unreachable!("CRC-64/XZ selected CRC hasher"),
        };
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

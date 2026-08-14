use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{BackendKind, Error, Result};

/// Launcher/API deployment region used by the supported official backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegionId {
    Cn,
    Sg,
    Kr,
    En,
    Jp,
}

impl RegionId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cn => "cn",
            Self::Sg => "sg",
            Self::Kr => "kr",
            Self::En => "en",
            Self::Jp => "jp",
        }
    }

    pub const fn backend(self) -> BackendKind {
        match self {
            Self::Cn | Self::Sg => BackendKind::Hypergryph,
            Self::Kr | Self::En | Self::Jp => BackendKind::Yostar,
        }
    }

    pub const fn hypergryph_official_channel_id(self) -> Option<&'static str> {
        match self {
            Self::Cn => Some("1"),
            Self::Sg => Some("6"),
            Self::Kr | Self::En | Self::Jp => None,
        }
    }
}

impl std::fmt::Display for RegionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RegionId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cn" | "china" | "mainland" => Ok(Self::Cn),
            "sg" | "global" | "os" | "overseas" => Ok(Self::Sg),
            "kr" | "korea" | "arknights-kr" => Ok(Self::Kr),
            "en" | "arknights-en" => Ok(Self::En),
            "jp" | "japan" | "arknights-jp" => Ok(Self::Jp),
            value => Err(Error::Message { context: "Configuration error: ", detail: format!(
                "invalid region {value:?}: expected cn, sg, kr, en, or jp (aliases: china/mainland, global/os/overseas, korea/arknights-kr, arknights-en, japan/arknights-jp)"
            ) }),
        }
    }
}

impl Serialize for RegionId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RegionId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_normalize_to_native_region_values() {
        for (input, expected) in [
            ("cn", RegionId::Cn),
            ("China", RegionId::Cn),
            ("sg", RegionId::Sg),
            ("global", RegionId::Sg),
            ("OS", RegionId::Sg),
            ("kr", RegionId::Kr),
            ("Korea", RegionId::Kr),
            ("en", RegionId::En),
            ("jp", RegionId::Jp),
            ("Japan", RegionId::Jp),
        ] {
            let parsed = input.parse::<RegionId>().unwrap();
            assert_eq!(parsed, expected);
            assert!(matches!(parsed.as_str(), "cn" | "sg" | "kr" | "en" | "jp"));
        }
    }
}

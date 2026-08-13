use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// Launcher/API deployment region as written by official `config.ini` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegionId {
    Cn,
    Sg,
    En,
}

impl RegionId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cn => "cn",
            Self::Sg => "sg",
            Self::En => "en",
        }
    }

    pub const fn official_channel_id(self) -> &'static str {
        match self {
            Self::Cn => "1",
            Self::Sg => "6",
            Self::En => "0",
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
            "en" | "yostar" | "arknights-en" => Ok(Self::En),
            value => Err(Error::Message { context: "Configuration error: ", detail: format!(
                "invalid region {value:?}: expected cn, sg, or en (aliases: china/mainland, global/os/overseas, yostar/arknights-en)"
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
            ("en", RegionId::En),
            ("YoStar", RegionId::En),
        ] {
            let parsed = input.parse::<RegionId>().unwrap();
            assert_eq!(parsed, expected);
            assert!(matches!(parsed.as_str(), "cn" | "sg" | "en"));
        }
    }
}

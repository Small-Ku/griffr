use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::RegionId;
use crate::error::{Error, Result};

const CN_OFFICIAL_ID: &str = "1";
const BILIBILI_ID: &str = "2";
const SG_OFFICIAL_ID: &str = "6";
const EPIC_ID: &str = "801";
const GOOGLE_PLAY_ID: &str = "802";

fn numeric_id(value: &str, field: &str) -> Result<ChannelId> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::Config(format!("{field} cannot be empty")));
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(ChannelId(Cow::Owned(value.to_string())));
    }
    Err(Error::Config(format!(
        "invalid {field} {value:?}: expected a numeric API ID or a supported alias"
    )))
}

fn parse_channel(region: RegionId, value: Option<String>) -> Result<ChannelId> {
    let Some(value) = value else {
        return Ok(ChannelId::known(region.official_channel_id()));
    };
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "official" => Ok(ChannelId::known(region.official_channel_id())),
        "bilibili" | "bili" => Ok(ChannelId::BILIBILI),
        _ => numeric_id(&value, "channel"),
    }
}

fn parse_sub_channel(
    region: RegionId,
    channel: &ChannelId,
    value: Option<String>,
) -> Result<ChannelId> {
    let Some(value) = value else {
        return Ok(channel.clone());
    };
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "official" => Ok(ChannelId::known(region.official_channel_id())),
        "bilibili" | "bili" => Ok(ChannelId::BILIBILI),
        "epic" | "epic-games" | "epic-store" | "egs" => Ok(ChannelId::EPIC),
        "google-play" | "googleplay" | "gplay" => Ok(ChannelId::GOOGLE_PLAY),
        _ => numeric_id(&value, "sub-channel"),
    }
}

/// One numeric launcher API channel or sub-channel identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(Cow<'static, str>);

impl ChannelId {
    pub const CN_OFFICIAL: Self = Self::known(CN_OFFICIAL_ID);
    pub const BILIBILI: Self = Self::known(BILIBILI_ID);
    pub const SG_OFFICIAL: Self = Self::known(SG_OFFICIAL_ID);
    pub const EPIC: Self = Self::known(EPIC_ID);
    pub const GOOGLE_PLAY: Self = Self::known(GOOGLE_PLAY_ID);

    pub const fn known(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        numeric_id(&value, "channel id")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ChannelId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

impl Serialize for ChannelId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ChannelId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Native launcher API `channel` and `sub_channel` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPair {
    channel: ChannelId,
    sub_channel: ChannelId,
}

impl ChannelPair {
    pub fn new(channel: ChannelId, sub_channel: Option<ChannelId>) -> Self {
        let sub_channel = sub_channel.unwrap_or_else(|| channel.clone());
        Self {
            channel,
            sub_channel,
        }
    }

    /// Parse optional CLI fields. Omitted `channel` means the region's official
    /// master channel; omitted `sub_channel` copies the resolved channel.
    pub fn parse(
        region: RegionId,
        channel: Option<String>,
        sub_channel: Option<String>,
    ) -> Result<Self> {
        let channel = parse_channel(region, channel)?;
        let sub_channel = parse_sub_channel(region, &channel, sub_channel)?;
        Ok(Self::new(channel, Some(sub_channel)))
    }

    /// Construct a pair from launcher metadata, where both fields are numeric.
    pub fn from_api(
        channel: impl Into<String>,
        sub_channel: Option<impl Into<String>>,
    ) -> Result<Self> {
        let channel = ChannelId::new(channel)?;
        let sub_channel = sub_channel.map(|value| ChannelId::new(value)).transpose()?;
        Ok(Self::new(channel, sub_channel))
    }

    pub fn channel(&self) -> &ChannelId {
        &self.channel
    }

    pub fn sub_channel(&self) -> &ChannelId {
        &self.sub_channel
    }

    pub fn into_parts(self) -> (ChannelId, ChannelId) {
        (self.channel, self.sub_channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_fields_select_native_official_tuple() {
        let cn = ChannelPair::parse(RegionId::Cn, None, None).unwrap();
        assert_eq!(cn.channel(), &ChannelId::CN_OFFICIAL);
        assert_eq!(cn.sub_channel(), &ChannelId::CN_OFFICIAL);

        let sg = ChannelPair::parse(RegionId::Sg, None, None).unwrap();
        assert_eq!(sg.channel(), &ChannelId::SG_OFFICIAL);
        assert_eq!(sg.sub_channel(), &ChannelId::SG_OFFICIAL);
    }

    #[test]
    fn scoped_aliases_preserve_api_field_semantics() {
        let bili = ChannelPair::parse(RegionId::Cn, Some("bili".into()), None).unwrap();
        assert_eq!(bili.channel(), &ChannelId::BILIBILI);
        assert_eq!(bili.sub_channel(), &ChannelId::BILIBILI);

        let gplay = ChannelPair::parse(RegionId::Sg, None, Some("google-play".into())).unwrap();
        assert_eq!(gplay.channel(), &ChannelId::SG_OFFICIAL);
        assert_eq!(gplay.sub_channel(), &ChannelId::GOOGLE_PLAY);
    }

    #[test]
    fn aliases_do_not_validate_game_or_region_combinations() {
        let pair = ChannelPair::parse(RegionId::Cn, None, Some("google-play".into())).unwrap();
        assert_eq!(pair.channel(), &ChannelId::CN_OFFICIAL);
        assert_eq!(pair.sub_channel(), &ChannelId::GOOGLE_PLAY);
    }

    #[test]
    fn raw_api_ids_are_preserved() {
        let pair =
            ChannelPair::parse(RegionId::Sg, Some("123".into()), Some("456".into())).unwrap();
        assert_eq!(pair.channel().as_str(), "123");
        assert_eq!(pair.sub_channel().as_str(), "456");
    }

    #[test]
    fn sub_channel_alias_is_not_accepted_as_master_channel_alias() {
        assert!(ChannelPair::parse(RegionId::Sg, Some("google-play".into()), None).is_err());
    }
}

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const HYPERGRYPH_ID: &str = "1";
const BILIBILI_ID: &str = "2";
const GRYPHLINE_ID: &str = "6";
const EPIC_STORE_ID: &str = "801";
const GOOGLE_PLAY_ID: &str = "802";

const CHANNEL_ALIASES: &[(&str, &str)] = &[
    ("hypergryph", HYPERGRYPH_ID),
    ("bilibili", BILIBILI_ID),
    ("gryphline", GRYPHLINE_ID),
    ("epic_store", EPIC_STORE_ID),
    ("google_play", GOOGLE_PLAY_ID),
];

fn normalize_value(value: String) -> Result<Cow<'static, str>> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::Config("channel id cannot be empty".to_string()));
    }

    if let Some((_, id)) = CHANNEL_ALIASES
        .iter()
        .find(|(alias, _)| value.eq_ignore_ascii_case(alias))
    {
        return Ok(Cow::Borrowed(id));
    }

    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(Cow::Owned(value.to_string()));
    }

    Err(Error::Config(format!(
        "invalid channel id {value:?}: expected a numeric ID or one of {}",
        CHANNEL_ALIASES
            .iter()
            .map(|(alias, _)| *alias)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// One API channel or sub-channel identifier.
///
/// Friendly aliases are normalized to their numeric value. Numeric IDs are
/// preserved verbatim for the server to validate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(Cow<'static, str>);

impl ChannelId {
    pub const HYPERGRYPH: Self = Self::known(HYPERGRYPH_ID);
    pub const BILIBILI: Self = Self::known(BILIBILI_ID);
    pub const GRYPHLINE: Self = Self::known(GRYPHLINE_ID);
    pub const EPIC_STORE: Self = Self::known(EPIC_STORE_ID);
    pub const GOOGLE_PLAY: Self = Self::known(GOOGLE_PLAY_ID);

    pub const fn known(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    pub fn new(value: impl Into<String>) -> Result<Self> {
        Ok(Self(normalize_value(value.into())?))
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

/// Channel and sub-channel values used together by API requests.
///
/// The two fields are normalized independently. Omitting the sub-channel only
/// applies the CLI/API default that it is equal to the channel; neither value
/// otherwise implies or rewrites the other.
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

    pub fn parse(
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
    fn channel_id_contains_one_normalized_value() {
        for (alias, expected) in [
            ("hypergryph", "1"),
            ("bilibili", "2"),
            ("gryphline", "6"),
            ("epic_store", "801"),
            ("google_play", "802"),
        ] {
            assert_eq!(ChannelId::new(alias).unwrap().as_str(), expected);
        }
        assert_eq!(ChannelId::new("00042").unwrap().as_str(), "00042");
    }

    #[test]
    fn invalid_text_is_rejected_before_server_validation() {
        assert!(ChannelId::new("future-channel").is_err());
        assert!(ChannelId::new("6/801").is_err());
    }

    #[test]
    fn pair_normalizes_both_ids_independently() {
        let pair = ChannelPair::parse("hypergryph", Some("google_play")).unwrap();
        assert_eq!(pair.channel(), &ChannelId::HYPERGRYPH);
        assert_eq!(pair.sub_channel(), &ChannelId::GOOGLE_PLAY);

        let reversed = ChannelPair::parse("epic_store", Some("bilibili")).unwrap();
        assert_eq!(reversed.channel(), &ChannelId::EPIC_STORE);
        assert_eq!(reversed.sub_channel(), &ChannelId::BILIBILI);
    }

    #[test]
    fn omitted_sub_channel_copies_only_the_normalized_channel() {
        let pair = ChannelPair::parse("epic_store", None::<String>).unwrap();
        assert_eq!(pair.channel(), &ChannelId::EPIC_STORE);
        assert_eq!(pair.sub_channel(), &ChannelId::EPIC_STORE);
    }
}

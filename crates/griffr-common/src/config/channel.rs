use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelId(std::borrow::Cow<'static, str>);

impl ChannelId {
    pub const CN_OFFICIAL: Self = Self(std::borrow::Cow::Borrowed("cn_official"));
    pub const CN_BILIBILI: Self = Self(std::borrow::Cow::Borrowed("cn_bilibili"));
    pub const GLOBAL_OFFICIAL: Self = Self(std::borrow::Cow::Borrowed("global_official"));
    pub const GLOBAL_EPIC: Self = Self(std::borrow::Cow::Borrowed("global_epic"));
    pub const GLOBAL_GOOGLEPLAY: Self = Self(std::borrow::Cow::Borrowed("global_googleplay"));

    pub const fn known(value: &'static str) -> Self {
        Self(std::borrow::Cow::Borrowed(value))
    }

    pub fn new(value: impl Into<String>) -> Self {
        Self(std::borrow::Cow::Owned(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ChannelId {
    fn default() -> Self {
        Self::CN_OFFICIAL
    }
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ChannelId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let value = s.trim().to_lowercase();
        if value.is_empty() {
            return Err(Error::Config("channel id cannot be empty".to_string()));
        }
        Ok(Self::new(value))
    }
}

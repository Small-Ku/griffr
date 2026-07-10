use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::{ChannelId, ChannelSettings};
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GameId(std::borrow::Cow<'static, str>);

impl GameId {
    pub const ARKNIGHTS: Self = Self(std::borrow::Cow::Borrowed("arknights"));
    pub const ENDFIELD: Self = Self(std::borrow::Cow::Borrowed("endfield"));

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

impl std::fmt::Display for GameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for GameId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let value = s.trim().to_lowercase();
        if value.is_empty() {
            return Err(Error::Game("game id cannot be empty".to_string()));
        }
        Ok(Self::new(value))
    }
}

/// Per-game configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameConfig {
    /// Installation path
    pub install_path: Option<PathBuf>,

    /// Currently active channel
    #[serde(default)]
    pub active_channel: ChannelId,

    /// Tracked version for this install
    pub version: Option<String>,

    /// Last update timestamp
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,

    /// Per-channel configurations
    #[serde(default)]
    pub channels: BTreeMap<ChannelId, ChannelSettings>,
}

impl GameConfig {
    /// Get the install path for a specific channel
    ///
    /// Returns the path configured for this exact channel.
    pub fn channel_install_path(&self, channel: ChannelId) -> Option<PathBuf> {
        self.channels
            .get(&channel)
            .and_then(|entry| entry.install_path.clone())
    }
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            install_path: None,
            active_channel: ChannelId::CN_OFFICIAL,
            version: None,
            last_update: None,
            channels: BTreeMap::new(),
        }
    }
}

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GameId(Cow<'static, str>);

impl GameId {
    pub const ARKNIGHTS: Self = Self(Cow::Borrowed("arknights"));
    pub const ENDFIELD: Self = Self(Cow::Borrowed("endfield"));

    pub const fn known(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    pub fn new(value: impl Into<String>) -> Self {
        Self(Cow::Owned(value.into()))
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
            return Err(Error::Message {
                context: "Game error: ",
                detail: "game id cannot be empty".to_string(),
            });
        }
        Ok(Self::new(value))
    }
}

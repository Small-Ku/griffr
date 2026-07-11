use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GameAppCode(pub String);

impl GameAppCode {
    pub fn new(val: impl Into<String>) -> Self {
        Self(val.into())
    }
}

impl std::fmt::Display for GameAppCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LauncherAppCode(pub String);

impl LauncherAppCode {
    pub fn new(val: impl Into<String>) -> Self {
        Self(val.into())
    }
}

impl std::fmt::Display for LauncherAppCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LauncherGateway(pub String);

impl LauncherGateway {
    pub fn new(val: impl Into<String>) -> Self {
        Self(val.into())
    }
}

impl std::fmt::Display for LauncherGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

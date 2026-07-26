use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runtime::paths::griffr_path;
use crate::runtime::task_pool::fs_ops::write_atomic_bytes;

pub const INSTALL_CHANGE_STATE_NAME: &str = "state.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallChangeKind {
    Install,
    Update,
    Repair,
}

impl std::fmt::Display for InstallChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Repair => "repair",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallChangeSource {
    FullArchive,
    PatchArchive,
    Reuse,
    Repair,
}

impl std::fmt::Display for InstallChangeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::FullArchive => "full_archive",
            Self::PatchArchive => "patch_archive",
            Self::Reuse => "reuse",
            Self::Repair => "repair",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallChangeState {
    pub schema_version: u32,
    pub kind: InstallChangeKind,
    pub source: InstallChangeSource,
    pub game: String,
    pub region: String,
    pub channel: String,
    pub sub_channel: String,
    pub from_version: Option<String>,
    pub target_version: String,
    pub game_files_md5: Option<String>,
    pub payload_md5s: Vec<String>,
    pub sync_vfs: bool,
    pub start_time: String,
}

impl InstallChangeState {
    pub const SCHEMA_VERSION: u32 = 2;

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: InstallChangeKind,
        source: InstallChangeSource,
        game: impl Into<String>,
        region: impl Into<String>,
        channel: impl Into<String>,
        sub_channel: impl Into<String>,
        from_version: Option<String>,
        target_version: impl Into<String>,
        game_files_md5: Option<String>,
        payload_md5s: Vec<String>,
        sync_vfs: bool,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            kind,
            source,
            game: game.into(),
            region: region.into(),
            channel: channel.into(),
            sub_channel: sub_channel.into(),
            from_version,
            target_version: target_version.into(),
            game_files_md5: game_files_md5
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.to_ascii_lowercase()),
            payload_md5s: payload_md5s
                .into_iter()
                .map(|value| value.to_ascii_lowercase())
                .collect(),
            sync_vfs,
            start_time: Utc::now().to_rfc3339(),
        }
    }

    pub fn state_path(install_root: &Path) -> PathBuf {
        griffr_path(install_root).join(INSTALL_CHANGE_STATE_NAME)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Unsupported install change schema version {}",
                    self.schema_version
                ),
            });
        }
        for (name, value) in [
            ("game", self.game.as_str()),
            ("region", self.region.as_str()),
            ("channel", self.channel.as_str()),
            ("sub_channel", self.sub_channel.as_str()),
            ("target_version", self.target_version.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!("Install change {name} cannot be empty"),
                });
            }
        }
        if self
            .from_version
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: "Install change from_version cannot be empty".to_string(),
            });
        }
        if matches!(
            self.kind,
            InstallChangeKind::Update | InstallChangeKind::Repair
        ) && self.from_version.is_none()
        {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!("Install change kind {} requires from_version", self.kind),
            });
        }
        if self.kind == InstallChangeKind::Repair
            && self.from_version.as_deref() != Some(self.target_version.as_str())
        {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: "Repair change must keep the same version".to_string(),
            });
        }
        let source_is_valid = match self.kind {
            InstallChangeKind::Install => matches!(
                self.source,
                InstallChangeSource::FullArchive | InstallChangeSource::Reuse
            ),
            InstallChangeKind::Update => true,
            InstallChangeKind::Repair => self.source == InstallChangeSource::Repair,
        };
        if !source_is_valid
            || (self.source == InstallChangeSource::PatchArchive
                && self.kind != InstallChangeKind::Update)
        {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Install change source {} is not valid for kind {}",
                    self.source, self.kind
                ),
            });
        }
        let _ = DateTime::parse_from_rfc3339(&self.start_time)?;
        if let Some(md5) = self.game_files_md5.as_deref() {
            validate_md5("game_files_md5", md5)?;
        }
        for md5 in &self.payload_md5s {
            validate_md5("payload_md5s", md5)?;
        }
        Ok(())
    }

    pub fn matches_install(
        &self,
        game: &str,
        region: &str,
        channel: &str,
        sub_channel: &str,
    ) -> bool {
        self.game == game
            && self.region == region
            && self.channel == channel
            && self.sub_channel == sub_channel
    }

    pub fn same_install(&self, other: &Self) -> bool {
        self.matches_install(
            &other.game,
            &other.region,
            &other.channel,
            &other.sub_channel,
        )
    }

    pub fn same_change(&self, other: &Self) -> bool {
        self.same_install(other)
            && self.kind == other.kind
            && self.source == other.source
            && self.from_version == other.from_version
            && self.target_version == other.target_version
            && self.game_files_md5 == other.game_files_md5
            && self.payload_md5s == other.payload_md5s
            && self.sync_vfs == other.sync_vfs
    }

    /// Return whether the live API response still identifies the same target
    /// release as this marker. The target version is always required. When the
    /// marker captured a `game_files` digest, the live manifest must keep the
    /// same digest as well; this prevents a stale marker from silently using a
    /// newer release manifest under the old target label.
    pub fn matches_release(&self, target_version: &str, game_files_md5: Option<&str>) -> bool {
        if self.target_version != target_version {
            return false;
        }
        match self.game_files_md5.as_deref() {
            Some(expected) => game_files_md5
                .map(str::to_ascii_lowercase)
                .is_some_and(|actual| actual == expected),
            None => true,
        }
    }

    fn can_advance_to(&self, next: &Self) -> bool {
        if !self.same_install(next) {
            return false;
        }
        match (self.kind, next.kind) {
            (InstallChangeKind::Install, InstallChangeKind::Install) => {
                self.target_version != next.target_version
                    || self.game_files_md5 == next.game_files_md5
            }
            (InstallChangeKind::Update, InstallChangeKind::Update)
                if self.from_version == next.from_version
                    && self.target_version == next.target_version =>
            {
                true
            }
            (InstallChangeKind::Repair, InstallChangeKind::Repair)
                if self.from_version == next.from_version
                    && self.target_version == next.target_version =>
            {
                true
            }
            (
                InstallChangeKind::Install | InstallChangeKind::Update | InstallChangeKind::Repair,
                InstallChangeKind::Update,
            ) => {
                self.target_version != next.target_version
                    && (next.from_version.as_deref() == Some(self.target_version.as_str())
                        || next.from_version == self.from_version)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallChangeStart {
    New,
    Resume,
    Advance,
}

pub fn read_install_change(install_root: &Path) -> Result<Option<InstallChangeState>> {
    let path = InstallChangeState::state_path(install_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::IoAt {
                action: "open file",
                path,
                source,
            });
        }
    };
    let state: InstallChangeState = serde_json::from_slice(&bytes)?;
    state.validate()?;
    Ok(Some(state))
}

pub fn ensure_install_ready(install_root: &Path) -> Result<()> {
    let Some(state) = read_install_change(install_root)? else {
        return Ok(());
    };
    Err(Error::Message {
        context: "Launcher/Process error: ",
        detail: format!(
            "Game launch is blocked while {} change {} -> {} is unfinished at {}",
            state.kind,
            state.from_version.as_deref().unwrap_or("none"),
            state.target_version,
            install_root.display()
        ),
    })
}

pub fn start_install_change(
    install_root: &Path,
    requested: &InstallChangeState,
) -> Result<InstallChangeStart> {
    requested.validate()?;
    match read_install_change(install_root)? {
        Some(current) if current.same_change(requested) => Ok(InstallChangeStart::Resume),
        Some(current) if current.can_advance_to(requested) => {
            write_install_change(install_root, requested)?;
            Ok(InstallChangeStart::Advance)
        }
        Some(current) => Err(Error::Message {
            context: "Install change error: ",
            detail: format!(
                "Pending {} change {} -> {} from {} conflicts with requested {} change {} -> {} from {}",
                current.kind,
                current.from_version.as_deref().unwrap_or("none"),
                current.target_version,
                current.source,
                requested.kind,
                requested.from_version.as_deref().unwrap_or("none"),
                requested.target_version,
                requested.source,
            ),
        }),
        None => {
            write_install_change(install_root, requested)?;
            Ok(InstallChangeStart::New)
        }
    }
}

pub fn finish_install_change(install_root: &Path, expected: &InstallChangeState) -> Result<()> {
    let Some(current) = read_install_change(install_root)? else {
        return Err(Error::Message {
            context: "Install change error: ",
            detail: format!(
                "Install change marker is missing for {} target {}",
                expected.kind, expected.target_version
            ),
        });
    };
    if !current.same_change(expected) {
        return Err(Error::Message {
            context: "Install change error: ",
            detail: format!(
                "Install change marker for {} target {} does not match requested {} target {}",
                current.kind, current.target_version, expected.kind, expected.target_version
            ),
        });
    }

    let state_path = InstallChangeState::state_path(install_root);
    remove_stale_state_temps(&state_path)?;
    std::fs::remove_file(&state_path).map_err(|source| Error::IoAt {
        action: "remove file or directory",
        path: state_path.clone(),
        source,
    })?;
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

fn write_install_change(install_root: &Path, state: &InstallChangeState) -> Result<()> {
    state.validate()?;
    let path = InstallChangeState::state_path(install_root);
    let payload = serde_json::to_vec_pretty(state)?;
    write_atomic_bytes(&path, &payload)
}

fn remove_stale_state_temps(state_path: &Path) -> Result<()> {
    let Some(parent) = state_path.parent() else {
        return Ok(());
    };
    let Some(file_name) = state_path.file_name() else {
        return Ok(());
    };
    let prefix = format!(".{}.griffr.tmp.", file_name.to_string_lossy());
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::IoAt {
                action: "read directory",
                path: parent.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| Error::IoAt {
            action: "read directory",
            path: parent.to_path_buf(),
            source,
        })?;
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        std::fs::remove_file(entry.path()).map_err(|source| Error::IoAt {
            action: "remove file or directory",
            path: entry.path(),
            source,
        })?;
    }
    Ok(())
}

fn validate_md5(field: &str, value: &str) -> Result<()> {
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(Error::Message {
        context: "Configuration error: ",
        detail: format!("Install change {field} contains invalid MD5 {value:?}"),
    })
}

#[cfg(test)]
mod tests;

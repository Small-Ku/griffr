use rapidhash::RapidHashMap as HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use crate::runtime::FileIssue;

use crate::api::ApiClient;
use crate::config::{ChannelId, ChannelSettings, GameConfig, GameId, InstallProfile};
use crate::runtime::task_pool::Task;

/// Manages game installation state and version tracking
#[derive(Debug)]
pub struct GameManager {
    game_id: GameId,
    pub(super) config: GameConfig,
    profile: InstallProfile,
}

#[derive(Debug, Clone, Default)]
pub struct IntegrityRunSummary {
    pub issues: Vec<FileIssue>,
    pub verified_files: usize,
    pub downloaded_files: usize,
    pub reused_files: usize,
}

impl GameManager {
    fn normalize_progress_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    pub(super) fn resolve_reused_logical_path(
        path: &Path,
        filename_index: &HashMap<String, Vec<String>>,
    ) -> Option<String> {
        let normalized = Self::normalize_progress_path(path);
        let filename = path.file_name()?.to_str()?;
        let candidates = filename_index.get(filename)?;
        candidates
            .iter()
            .find(|candidate| normalized.ends_with(candidate.as_str()))
            .cloned()
    }

    pub(super) fn task_progress_path(task: &Task) -> Option<&str> {
        match task {
            Task::Download { logical_path, .. }
            | Task::Verify { logical_path, .. }
            | Task::EnsureFile { logical_path, .. } => Some(logical_path.as_str()),
            _ => None,
        }
    }

    pub(super) fn task_expected_bytes(task: &Task) -> u64 {
        match task {
            Task::Download { expected_size, .. } => expected_size.unwrap_or(0),
            Task::EnsureFile { expected_size, .. } => *expected_size,
            _ => 0,
        }
    }

    /// Create a new game manager for the given game configuration
    pub fn new(game_id: GameId, config: GameConfig, profile: InstallProfile) -> Self {
        Self {
            game_id,
            config,
            profile,
        }
    }

    pub(super) fn derive_files_base_url(file_path: &str) -> String {
        let normalized = file_path.trim_end_matches('/');
        normalized
            .strip_suffix("/game_files")
            .unwrap_or(normalized)
            .to_string()
    }

    pub(super) fn derive_game_files_url(file_path: &str) -> String {
        let normalized = file_path.trim_end_matches('/');
        if normalized.ends_with("/game_files") {
            normalized.to_string()
        } else {
            format!("{}/game_files", normalized)
        }
    }

    /// Get the game ID
    pub fn game_id(&self) -> GameId {
        self.game_id.clone()
    }

    pub fn active_install_profile(&self) -> Result<InstallProfile> {
        Ok(self.profile.clone())
    }

    /// Get the installation path configured for the active channel.
    pub fn install_path(&self) -> Option<&Path> {
        self.config
            .channels
            .get(&self.config.active_channel)
            .and_then(|channel| channel.install_path.as_deref())
    }

    /// Set the installation path
    pub fn set_install_path(&mut self, path: impl Into<PathBuf>) {
        let active = self.config.active_channel.clone();
        self.config.channels.entry(active).or_default().install_path = Some(path.into());
    }

    /// Get the active channel
    pub fn active_channel(&self) -> ChannelId {
        self.config.active_channel.clone()
    }

    /// Set the active channel
    pub fn set_active_channel(&mut self, channel: ChannelId) {
        self.config.active_channel = channel;
    }

    /// Get the currently installed version (for active channel)
    pub fn current_version(&self) -> Option<&str> {
        self.config
            .channels
            .get(&self.config.active_channel)
            .and_then(|channel| channel.version.as_deref())
    }

    /// Set the installed version for the active channel.
    pub fn set_version(&mut self, version: impl Into<String>) {
        let version = version.into();
        let active = self.config.active_channel.clone();
        self.config.channels.entry(active).or_default().version = Some(version.clone());
        self.config.version = Some(version);
    }

    /// Get the channel configuration
    pub fn channel_config(&self, channel: ChannelId) -> Option<&ChannelSettings> {
        self.config.channels.get(&channel)
    }

    /// Get mutable channel configuration
    pub fn channel_config_mut(&mut self, channel: ChannelId) -> &mut ChannelSettings {
        self.config.channels.entry(channel).or_default()
    }

    /// Mark a channel as installed
    pub fn mark_channel_installed(&mut self, channel: ChannelId, version: impl Into<String>) {
        let version = version.into();
        let is_active = channel == self.config.active_channel;

        // Extract install_path before mutating so we don't hold a borrow across the assignment
        let channel_install_path = {
            let channel_config = self.channel_config_mut(channel);
            channel_config.installed = true;
            channel_config.version = Some(version.clone());
            channel_config.last_update = Some(chrono::Utc::now());
            channel_config.install_path.clone()
        }; // mutable borrow ends here

        // Keep the "current version" and install_path in sync for the active channel.
        if is_active {
            self.config.version = Some(version);
            if let Some(path) = channel_install_path {
                self.config.install_path = Some(path);
            }
        }
    }

    /// Check if the game is installed (has an install path set)
    pub fn is_installed(&self) -> bool {
        self.install_path().is_some()
    }

    /// Check if the active channel is installed
    pub fn is_active_channel_installed(&self) -> bool {
        self.config
            .channels
            .get(&self.config.active_channel)
            .map(|s| s.installed)
            .unwrap_or(false)
    }

    /// Get the path to the game executable
    pub fn game_exe_path(&self) -> Option<PathBuf> {
        let install_path = self.install_path()?;
        let profile = self.active_install_profile().ok()?;
        Some(install_path.join(profile.executable))
    }

    /// Get the path to the config.ini file
    pub fn config_ini_path(&self) -> Option<PathBuf> {
        let install_path = self.install_path()?;
        Some(install_path.join("config.ini"))
    }

    /// Read version from config.ini
    ///
    /// The config.ini file is AES-256-CBC encrypted (same key/IV as game_files
    /// manifest), so we decrypt it before parsing the version line.
    pub async fn read_version_from_ini(&self) -> Result<Option<String>> {
        let ini_path = match self.config_ini_path() {
            Some(path) => path,
            None => return Ok(None),
        };

        match compio::fs::metadata(&ini_path).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(Error::StatFailed {
                    path: ini_path.clone(),
                    source: err,
                })
            }
        }

        // config.ini is encrypted with the same AES-256-CBC key/IV as game_files
        let encrypted = compio::fs::read(&ini_path)
            .await
            .map_err(|e| Error::OpenFileFailed {
                path: ini_path.clone(),
                source: e,
            })?;

        let content = crate::api::crypto::decrypt_game_files(&encrypted)
            .map_err(|e| Error::Crypto(format!("Failed to decrypt config.ini: {e}")))?;

        // Parse version from decrypted config.ini
        // Format is typically: version=1.1.9
        for line in content.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("version=") {
                return Ok(Some(value.to_string()));
            }
        }

        Ok(None)
    }

    /// Write config.ini with the correct channel/sub_channel for the active channel
    ///
    /// This creates/updates the encrypted config.ini file that the official launcher
    /// uses to identify the channel channel. Without this file, the official launcher
    /// will not recognize the installation and may attempt to redownload all files.
    pub async fn write_config_ini(&self) -> Result<()> {
        let ini_path = match self.config_ini_path() {
            Some(path) => path,
            None => {
                return Err(Error::Config(
                    "Cannot determine config.ini path - game not installed".to_string(),
                ))
            }
        };

        let content = self.build_config_ini_content().await?;

        // Encrypt with AES-256-CBC
        let encrypted = crate::api::crypto::encrypt_game_files(content.as_bytes())
            .map_err(|e| Error::Crypto(format!("Failed to encrypt config.ini content: {e}")))?;

        // Write to file
        let write_result = compio::fs::write(&ini_path, encrypted).await;
        write_result.0.map_err(|e| Error::WriteFileFailed {
            path: ini_path,
            source: e,
        })?;

        Ok(())
    }

    fn uninstall_params(&self) -> Result<&'static str> {
        if self.game_id == GameId::ARKNIGHTS {
            Ok("{}")
        } else if self.game_id == GameId::ENDFIELD {
            Ok(r#"{"uninstall_path": "AntiCheatExpert/ACE-Setup64.exe", "uninstall_params": "-q"}"#)
        } else {
            Err(Error::Config(format!(
                "Unsupported game ID for uninstall params: {}",
                self.game_id
            )))
        }
    }

    async fn build_config_ini_content(&self) -> Result<String> {
        let install_path = self
            .install_path()
            .ok_or_else(|| Error::Config("Game not installed".to_string()))?;
        let version = self
            .current_version()
            .map(|v| v.to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "1.0.0".to_string());

        let profile = self.active_install_profile()?;
        let entry = profile.executable.to_string_lossy();
        let entry_md5 = self
            .calculate_file_md5(&install_path.join(&*profile.executable))
            .await?;

        let gateway_str = profile.target.gateway.0.to_string();
        let region_name = if gateway_str.contains("gryphline") {
            "sg"
        } else {
            "cn"
        };
        let appcode = &profile.target.game_appcode.0;
        let channel = profile.target.channel.as_str();
        let sub_channel = profile.target.sub_channel.as_str();

        let uninstall_params = self
            .uninstall_params()?
            .replace('\\', "\\\\")
            .replace('"', "\\\"");

        Ok(format!(
            "[Game]\nversion={version}\nentry={entry}\nentry_md5={entry_md5}\nappcode={appcode}\nregion={region_name}\nchannel={channel}\nsub_channel={sub_channel}\nuninstall_params=\"{uninstall_params}\"\n",
        ))
    }

    /// Sync launcher-facing metadata files in the install root.
    ///
    /// GRYPHLINE / Hypergryph Launcher tracks installs using more than the
    /// unpacked game files. Keeping these metadata files aligned with the
    /// current channel/version prevents the official launcher from treating a
    /// repaired install as foreign and scheduling a full redownload.
    pub async fn sync_launcher_metadata(&self, api_client: &ApiClient) -> Result<()> {
        let install_path = self
            .install_path()
            .ok_or_else(|| Error::Config("Game not installed".to_string()))?;
        let profile = self.active_install_profile()?;
        let version_info = api_client
            .get_latest_game(&profile.target, self.current_version())
            .await?;
        let pkg = version_info
            .pkg
            .as_ref()
            .ok_or_else(|| Error::ApiClient("No package information available".to_string()))?;

        let files_base_url = Self::derive_files_base_url(&pkg.file_path);
        let config_ini_url = format!("{}/config.ini", files_base_url);
        let config_ini_path = install_path.join("config.ini");
        api_client
            .download_file(&config_ini_url, &config_ini_path, false)
            .await
            .map_err(|e| {
                Error::Download(format!("Failed to sync launcher config.ini metadata: {e}"))
            })?;

        let game_files_url = Self::derive_game_files_url(&pkg.file_path);
        let game_files_path = install_path.join("game_files");
        if let Some(expected_md5) = pkg.game_files_md5.as_deref() {
            api_client
                .download_file_with_verify(&game_files_url, &game_files_path, expected_md5)
                .await
                .map_err(|e| {
                    Error::Download(format!("Failed to sync launcher game_files metadata: {e}"))
                })?;
        } else {
            api_client
                .download_file(&game_files_url, &game_files_path, false)
                .await
                .map_err(|e| {
                    Error::Download(format!("Failed to sync launcher game_files metadata: {e}"))
                })?;
        }

        let package_files_url = format!("{}/package_files", files_base_url);
        let package_files_path = install_path.join("package_files");
        let _ = api_client
            .download_file(&package_files_url, &package_files_path, false)
            .await;

        Ok(())
    }
}

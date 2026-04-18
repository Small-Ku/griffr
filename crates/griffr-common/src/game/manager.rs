//! Game installation and version management

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::{fs::File, io::Read};

use anyhow::{Context, Result};
use md5::{Digest, Md5};

use super::FileIssue;
use crate::api::types::{ChannelConfig, Game, Region};
use crate::api::ApiClient;
use crate::config::{GameConfig, GameId, ServerConfig, ServerId};
use crate::game::task_pool::{run_tasks, ProgressEvent, Task, TaskPoolConfig};

/// Manages game installation state and version tracking
#[derive(Debug)]
pub struct GameManager {
    game_id: GameId,
    config: GameConfig,
}

#[derive(Debug, Clone, Default)]
pub struct IntegrityRunSummary {
    pub issues: Vec<FileIssue>,
    pub verified_files: usize,
    pub downloaded_files: usize,
    pub reused_files: usize,
}

impl GameManager {
    /// Create a new game manager for the given game configuration
    pub fn new(game_id: GameId, config: GameConfig) -> Self {
        Self { game_id, config }
    }

    fn derive_files_base_url(file_path: &str) -> String {
        let normalized = file_path.trim_end_matches('/');
        normalized
            .strip_suffix("/game_files")
            .unwrap_or(normalized)
            .to_string()
    }

    fn derive_game_files_url(file_path: &str) -> String {
        let normalized = file_path.trim_end_matches('/');
        if normalized.ends_with("/game_files") {
            normalized.to_string()
        } else {
            format!("{}/game_files", normalized)
        }
    }

    /// Get the game ID
    pub fn game_id(&self) -> GameId {
        self.game_id
    }

    /// Get the installation path for the active server
    ///
    /// Checks the active server's own install_path first, then falls back
    /// to the game-level install_path for backward compatibility.
    pub fn install_path(&self) -> Option<&Path> {
        self.config
            .servers
            .get(&self.config.active_server)
            .and_then(|s| s.install_path.as_deref())
            .or(self.config.install_path.as_deref())
    }

    /// Set the installation path
    pub fn set_install_path(&mut self, path: impl Into<PathBuf>) {
        self.config.install_path = Some(path.into());
    }

    /// Get the active server
    pub fn active_server(&self) -> ServerId {
        self.config.active_server
    }

    /// Set the active server
    pub fn set_active_server(&mut self, server: ServerId) {
        self.config.active_server = server;
    }

    /// Get the currently installed version (for active server)
    pub fn current_version(&self) -> Option<&str> {
        self.config.version.as_deref()
    }

    /// Set the installed version
    pub fn set_version(&mut self, version: impl Into<String>) {
        self.config.version = Some(version.into());
    }

    /// Get the server configuration
    pub fn server_config(&self, server: ServerId) -> Option<&ServerConfig> {
        self.config.servers.get(&server)
    }

    /// Get mutable server configuration
    pub fn server_config_mut(&mut self, server: ServerId) -> &mut ServerConfig {
        self.config.servers.entry(server).or_default()
    }

    /// Mark a server as installed
    pub fn mark_server_installed(&mut self, server: ServerId, version: impl Into<String>) {
        let version = version.into();
        let is_active = server == self.config.active_server;

        // Extract install_path before mutating so we don't hold a borrow across the assignment
        let server_install_path = {
            let server_config = self.server_config_mut(server);
            server_config.installed = true;
            server_config.version = Some(version.clone());
            server_config.last_update = Some(chrono::Utc::now());
            server_config.install_path.clone()
        }; // mutable borrow ends here

        // Keep the "current version" and install_path in sync for the active server.
        if is_active {
            self.config.version = Some(version);
            if let Some(path) = server_install_path {
                self.config.install_path = Some(path);
            }
        }
    }

    /// Check if the game is installed (has an install path set)
    pub fn is_installed(&self) -> bool {
        self.install_path().is_some()
    }

    /// Check if the active server is installed
    pub fn is_active_server_installed(&self) -> bool {
        self.config
            .servers
            .get(&self.config.active_server)
            .map(|s| s.installed)
            .unwrap_or(false)
    }

    /// Get the path to the game executable
    pub fn game_exe_path(&self) -> Option<PathBuf> {
        let install_path = self.install_path()?;
        let exe_name = match self.game_id {
            GameId::Arknights => "Arknights.exe",
            GameId::Endfield => "Endfield.exe",
        };
        Some(install_path.join(exe_name))
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
                return Err(err).with_context(|| {
                    format!("Failed to stat config.ini at {}", ini_path.display())
                })
            }
        }

        // config.ini is encrypted with the same AES-256-CBC key/IV as game_files
        let encrypted = compio::fs::read(&ini_path)
            .await
            .with_context(|| format!("Failed to read config.ini from {}", ini_path.display()))?;

        let content = crate::api::crypto::decrypt_game_files(&encrypted)
            .context("Failed to decrypt config.ini")?;

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

    /// Write config.ini with the correct channel/sub_channel for the active server
    ///
    /// This creates/updates the encrypted config.ini file that the official launcher
    /// uses to identify the server channel. Without this file, the official launcher
    /// will not recognize the installation and may attempt to redownload all files.
    pub async fn write_config_ini(&self) -> Result<()> {
        let ini_path = match self.config_ini_path() {
            Some(path) => path,
            None => anyhow::bail!("Cannot determine config.ini path - game not installed"),
        };

        let content = self.build_config_ini_content().await?;

        // Encrypt with AES-256-CBC
        let encrypted = crate::api::crypto::encrypt_game_files(content.as_bytes())
            .context("Failed to encrypt config.ini content")?;

        // Write to file
        let write_result = compio::fs::write(&ini_path, encrypted).await;
        write_result
            .0
            .with_context(|| format!("Failed to write config.ini to {}", ini_path.display()))?;

        Ok(())
    }

    fn game_api_info(&self) -> (Game, Region, &'static str) {
        match (self.game_id, self.active_server()) {
            (GameId::Arknights, _) => (Game::Arknights, Region::CN, "cn"),
            (GameId::Endfield, ServerId::CnOfficial | ServerId::CnBilibili) => {
                (Game::Endfield, Region::CN, "cn")
            }
            (GameId::Endfield, ServerId::GlobalOfficial | ServerId::GlobalEpic) => {
                (Game::Endfield, Region::OS, "sg")
            }
        }
    }

    fn uninstall_params(&self) -> &'static str {
        match self.game_id {
            GameId::Arknights => "{}",
            GameId::Endfield => {
                r#"{"uninstall_path": "AntiCheatExpert/ACE-Setup64.exe", "uninstall_params": "-q"}"#
            }
        }
    }

    async fn build_config_ini_content(&self) -> Result<String> {
        let install_path = self.install_path().context("Game not installed")?;
        let version = self
            .current_version()
            .map(|v| v.to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "1.0.0".to_string());

        let entry = match self.game_id {
            GameId::Arknights => "Arknights.exe",
            GameId::Endfield => "Endfield.exe",
        };
        let entry_md5 = self.calculate_file_md5(&install_path.join(entry)).await?;

        let channel_config = ChannelConfig::for_game_server(self.game_id, self.active_server());
        let (game, region, region_name) = self.game_api_info();
        let appcode = game.app_code(region);
        let uninstall_params = self
            .uninstall_params()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");

        Ok(format!(
            "[Game]\nversion={version}\nentry={entry}\nentry_md5={entry_md5}\nappcode={appcode}\nregion={region_name}\nchannel={channel}\nsub_channel={sub_channel}\nuninstall_params=\"{uninstall_params}\"\n",
            channel = channel_config.channel,
            sub_channel = channel_config.sub_channel,
        ))
    }

    /// Sync launcher-facing metadata files in the install root.
    ///
    /// GRYPHLINE / Hypergryph Launcher tracks installs using more than the
    /// unpacked game files. Keeping these metadata files aligned with the
    /// current server/version prevents the official launcher from treating a
    /// repaired install as foreign and scheduling a full redownload.
    pub async fn sync_launcher_metadata(&self, api_client: &ApiClient) -> Result<()> {
        let install_path = self.install_path().context("Game not installed")?;
        let server_id = self.active_server();
        let version_info = api_client
            .get_latest_game(self.game_id, server_id, self.current_version())
            .await?;
        let pkg = version_info
            .pkg
            .as_ref()
            .context("No package information available")?;

        let files_base_url = Self::derive_files_base_url(&pkg.file_path);
        let config_ini_url = format!("{}/config.ini", files_base_url);
        let config_ini_path = install_path.join("config.ini");
        api_client
            .download_file(&config_ini_url, &config_ini_path, false)
            .await
            .context("Failed to sync launcher config.ini metadata")?;

        let game_files_url = Self::derive_game_files_url(&pkg.file_path);
        let game_files_path = install_path.join("game_files");
        if let Some(expected_md5) = pkg.game_files_md5.as_deref() {
            api_client
                .download_file_with_verify(&game_files_url, &game_files_path, expected_md5)
                .await
                .context("Failed to sync launcher game_files metadata")?;
        } else {
            api_client
                .download_file(&game_files_url, &game_files_path, false)
                .await
                .context("Failed to sync launcher game_files metadata")?;
        }

        let package_files_url = format!("{}/package_files", files_base_url);
        let package_files_path = install_path.join("package_files");
        let _ = api_client
            .download_file(&package_files_url, &package_files_path, false)
            .await;

        Ok(())
    }

    /// Verify integrity of game files
    pub async fn run_integrity_pool(
        &self,
        api_client: &ApiClient,
        repair: bool,
        source_roots: &[PathBuf],
        allow_copy_fallback: bool,
        progress_callback: Option<impl Fn(usize, usize, &str)>,
    ) -> Result<IntegrityRunSummary> {
        let install_path = self.install_path().context("Game not installed")?;

        // Fetch version info for the version currently installed on disk so updates can
        // verify either a freshly extracted full package or a freshly applied patch.
        let server_id = self.active_server();
        let version_info = api_client
            .get_latest_game(self.game_id, server_id, self.current_version())
            .await?;

        let pkg = version_info
            .pkg
            .as_ref()
            .context("No package information available")?;

        // Fetch and decrypt game_files manifest
        let entries = api_client
            .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
            .await?;
        let files_base_url = pkg.file_path.trim_end_matches("/game_files");

        let tasks = entries
            .iter()
            .map(|entry| {
                if repair {
                    let source_candidates = source_roots
                        .iter()
                        .map(|root| root.join(&entry.path))
                        .collect::<Vec<_>>();
                    Task::EnsureFile {
                        dest: install_path.join(&entry.path),
                        logical_path: entry.path.clone(),
                        expected_md5: entry.md5.clone(),
                        expected_size: entry.size,
                        source_candidates,
                        download_url: Some(format!("{}/{}", files_base_url, entry.path)),
                        allow_copy_fallback,
                        retry_count: 0,
                    }
                } else {
                    Task::Verify {
                        path: install_path.join(&entry.path),
                        logical_path: entry.path.clone(),
                        expected_md5: entry.md5.clone(),
                        expected_size: Some(entry.size),
                        on_fail: None,
                    }
                }
            })
            .collect::<Vec<_>>();
        let total = tasks.len();
        let result = run_tasks(tasks, TaskPoolConfig::default())?;

        let mut issues = Vec::new();
        let mut finished = 0usize;
        let mut downloaded_paths = HashSet::new();
        let mut reused_paths = HashSet::new();
        for event in result.events {
            match event {
                ProgressEvent::Verified { path, issue, .. } => {
                    if let Some(ref cb) = progress_callback {
                        cb(finished, total, &path);
                    }
                    finished += 1;
                    if let Some(issue) = issue {
                        issues.push(issue);
                    }
                }
                ProgressEvent::Downloaded { path, .. } => {
                    downloaded_paths.insert(path);
                }
                ProgressEvent::Hardlinked { path } | ProgressEvent::Copied { path } => {
                    reused_paths.insert(path);
                }
                ProgressEvent::Failed { path, reason } => {
                    tracing::warn!("verify failed for {}: {}", path, reason);
                }
                _ => {}
            }
        }

        Ok(IntegrityRunSummary {
            issues,
            verified_files: finished,
            downloaded_files: downloaded_paths.len(),
            reused_files: reused_paths.len(),
        })
    }

    /// Verify integrity of game files
    pub async fn verify_integrity(
        &self,
        api_client: &ApiClient,
        progress_callback: Option<impl Fn(usize, usize, &str)>,
    ) -> Result<Vec<FileIssue>> {
        Ok(self
            .run_integrity_pool(api_client, false, &[], false, progress_callback)
            .await?
            .issues)
    }

    /// Calculate file MD5 hash
    async fn calculate_file_md5(&self, path: &Path) -> Result<String> {
        let mut file = File::open(path)?;
        let mut hasher = Md5::new();
        let mut buffer = vec![0; 8192];

        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Consume the manager and return the updated config
    pub fn into_config(self) -> GameConfig {
        self.config
    }

    /// Get a reference to the config
    pub fn config(&self) -> &GameConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_game_manager() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Endfield, config);

        assert!(!manager.is_installed());
        assert!(manager.install_path().is_none());

        manager.set_install_path("C:\\Games\\Endfield");
        assert!(manager.is_installed());
        assert_eq!(
            manager.install_path(),
            Some(Path::new("C:\\Games\\Endfield"))
        );

        assert_eq!(manager.active_server(), ServerId::CnOfficial);

        manager.set_active_server(ServerId::CnBilibili);
        assert_eq!(manager.active_server(), ServerId::CnBilibili);
    }

    #[test]
    fn test_game_manager_arknights() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Arknights, config);

        manager.set_install_path("C:\\Games\\Arknights");
        assert_eq!(manager.game_id(), GameId::Arknights);

        // Check exe path
        let exe_path = manager.game_exe_path();
        assert!(exe_path.is_some());
        assert!(exe_path
            .unwrap()
            .to_string_lossy()
            .contains("Arknights.exe"));
    }

    #[test]
    fn test_game_manager_endfield() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Endfield, config);

        manager.set_install_path("C:\\Games\\Endfield");

        // Check exe path
        let exe_path = manager.game_exe_path();
        assert!(exe_path.is_some());
        assert!(exe_path.unwrap().to_string_lossy().contains("Endfield.exe"));

        // Check config.ini path
        let ini_path = manager.config_ini_path();
        assert!(ini_path.is_some());
        assert!(ini_path.unwrap().to_string_lossy().contains("config.ini"));
    }

    #[test]
    fn test_server_installation() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Endfield, config);

        manager.set_install_path("C:\\Games\\Endfield");
        manager.mark_server_installed(ServerId::CnOfficial, "1.1.9");

        assert!(manager.is_active_server_installed());
        assert_eq!(manager.current_version(), Some("1.1.9"));

        // Check server config
        let server_config = manager.server_config(ServerId::CnOfficial);
        assert!(server_config.is_some());
        let server_config = server_config.unwrap();
        assert!(server_config.installed);
        assert_eq!(server_config.version, Some("1.1.9".to_string()));
        assert!(server_config.last_update.is_some());
    }

    #[test]
    fn test_multiple_servers() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Endfield, config);
        manager.set_install_path("C:\\Games\\Endfield");

        // Install CN Official
        manager.mark_server_installed(ServerId::CnOfficial, "1.1.9");

        // Switch to Bilibili and install
        manager.set_active_server(ServerId::CnBilibili);
        manager.mark_server_installed(ServerId::CnBilibili, "1.1.9");

        // Both should be marked installed
        assert!(
            manager
                .server_config(ServerId::CnOfficial)
                .unwrap()
                .installed
        );
        assert!(
            manager
                .server_config(ServerId::CnBilibili)
                .unwrap()
                .installed
        );

        // Active version should be Bilibili's
        assert_eq!(manager.current_version(), Some("1.1.9"));
    }

    #[test]
    fn test_version_tracking() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Endfield, config);

        manager.set_version("1.0.0");
        assert_eq!(manager.current_version(), Some("1.0.0"));

        manager.set_version("2.0.0");
        assert_eq!(manager.current_version(), Some("2.0.0"));
    }

    #[test]
    fn test_into_config() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Endfield, config);

        manager.set_install_path("C:\\Games\\Endfield");
        manager.set_version("1.1.9");
        manager.set_active_server(ServerId::CnBilibili);

        let config = manager.into_config();
        assert_eq!(
            config.install_path,
            Some(PathBuf::from("C:\\Games\\Endfield"))
        );
        assert_eq!(config.version, Some("1.1.9".to_string()));
        assert_eq!(config.active_server, ServerId::CnBilibili);
    }

    #[test]
    fn test_server_config_mut() {
        let config = GameConfig::default();
        let mut manager = GameManager::new(GameId::Endfield, config);

        // Get or create server config
        let server_config = manager.server_config_mut(ServerId::CnOfficial);
        server_config.installed = true;
        server_config.version = Some("1.0.0".to_string());

        assert!(
            manager
                .server_config(ServerId::CnOfficial)
                .unwrap()
                .installed
        );
    }

    #[compio::test]
    async fn test_write_config_ini_uses_launcher_format() {
        let temp = tempfile::tempdir().unwrap();
        let exe_path = temp.path().join("Endfield.exe");
        std::fs::write(&exe_path, b"endfield exe bytes").unwrap();

        let mut config = GameConfig {
            install_path: Some(temp.path().to_path_buf()),
            active_server: ServerId::GlobalOfficial,
            version: Some("1.2.4".to_string()),
            last_update: None,
            servers: Default::default(),
        };
        let server = config.servers.entry(ServerId::GlobalOfficial).or_default();
        server.installed = true;
        server.install_path = Some(temp.path().to_path_buf());
        server.version = Some("1.2.4".to_string());

        let manager = GameManager::new(GameId::Endfield, config);
        manager.write_config_ini().await.unwrap();

        let encrypted = std::fs::read(temp.path().join("config.ini")).unwrap();
        let decrypted = crate::api::crypto::decrypt_game_files(&encrypted).unwrap();

        assert!(decrypted.starts_with("[Game]\n"));
        assert!(decrypted.contains("version=1.2.4\n"));
        assert!(decrypted.contains("entry=Endfield.exe\n"));
        assert!(decrypted.contains("appcode=YDUTE5gscDZ229CW\n"));
        assert!(decrypted.contains("region=sg\n"));
        assert!(decrypted.contains("channel=6\n"));
        assert!(decrypted.contains("sub_channel=6\n"));
        assert!(decrypted.contains(
            "uninstall_params=\"{\\\"uninstall_path\\\": \\\"AntiCheatExpert/ACE-Setup64.exe\\\", \\\"uninstall_params\\\": \\\"-q\\\"}\"\n"
        ));
        assert!(decrypted.contains("entry_md5="));
    }

    #[test]
    fn test_derive_files_base_url_handles_game_files_suffix() {
        let url = "https://cdn.example.com/path/files/game_files";
        assert_eq!(
            GameManager::derive_files_base_url(url),
            "https://cdn.example.com/path/files"
        );
    }

    #[test]
    fn test_derive_files_base_url_handles_files_url() {
        let url = "https://cdn.example.com/path/files";
        assert_eq!(
            GameManager::derive_files_base_url(url),
            "https://cdn.example.com/path/files"
        );
    }

    #[test]
    fn test_derive_game_files_url_handles_both_shapes() {
        let files = "https://cdn.example.com/path/files";
        let game_files = "https://cdn.example.com/path/files/game_files";
        assert_eq!(
            GameManager::derive_game_files_url(files),
            "https://cdn.example.com/path/files/game_files"
        );
        assert_eq!(
            GameManager::derive_game_files_url(game_files),
            "https://cdn.example.com/path/files/game_files"
        );
    }
}

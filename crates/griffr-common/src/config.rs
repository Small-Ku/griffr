use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Current configuration format version.
/// NOTE: During pre-release (v0.x), the schema is subject to breaking changes.
/// Increment this version when making schema changes that require manual
/// intervention or automated migration.
const CONFIG_VERSION: u32 = 0;

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Config format version
    pub version: u32,

    /// Game configurations keyed by game ID
    pub games: HashMap<GameId, GameConfig>,

    /// Default settings
    #[serde(default)]
    pub defaults: DefaultSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            games: HashMap::new(),
            defaults: DefaultSettings::default(),
        }
    }
}

impl Config {
    /// Load configuration from the default location
    pub async fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path).await
    }

    /// Load configuration from a specific path
    pub async fn load_from(path: &Path) -> Result<Self> {
        match compio::fs::metadata(path).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to stat config at {}", path.display()))
            }
        }

        let bytes = compio::fs::read(path)
            .await
            .with_context(|| format!("Failed to read config from {}", path.display()))?;
        let content =
            String::from_utf8(bytes).context("Failed to decode config file as UTF-8 text")?;

        let config: Config =
            serde_json::from_str(&content).with_context(|| "Failed to parse config JSON")?;

        config.migrate()
    }

    /// Save configuration to the default location
    pub async fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path).await
    }

    /// Save configuration to a specific path
    pub async fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            compio::fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }

        let content = serde_json::to_string_pretty(self)?.into_bytes();
        let write_result = compio::fs::write(path, content).await;
        write_result
            .0
            .with_context(|| format!("Failed to write config to {}", path.display()))?;

        Ok(())
    }

    /// Get the default configuration file path
    pub fn config_path() -> Result<PathBuf> {
        let data_dir =
            dirs::data_local_dir().context("Failed to determine local data directory")?;
        Ok(data_dir.join("GriffrLauncher").join("config.json"))
    }

    /// Migrate configuration to the latest version
    fn migrate(self) -> Result<Self> {
        let mut config = self;

        // Migration: move GameConfig.install_path into each installed server's
        // ServerConfig.install_path so each server can have its own path.
        // Previously all servers shared one path; after migration each gets a copy.
        for game_config in config.games.values_mut() {
            if let Some(shared_path) = game_config.install_path.take() {
                // Copy the shared path to every installed server that doesn't
                // already have its own install_path.
                for server_config in game_config.servers.values_mut() {
                    if server_config.installed && server_config.install_path.is_none() {
                        server_config.install_path = Some(shared_path.clone());
                    }
                }
                // Restore game-level install_path from the active server (or the shared path)
                let active = game_config.active_server;
                game_config.install_path = game_config
                    .servers
                    .get(&active)
                    .and_then(|s| s.install_path.clone())
                    .or(Some(shared_path));
            }
        }

        config.version = CONFIG_VERSION;
        Ok(config)
    }

    /// Get or create a game configuration
    pub fn game_mut(&mut self, game_id: GameId) -> &mut GameConfig {
        self.games.entry(game_id).or_default()
    }

    /// Get a game configuration
    pub fn game(&self, game_id: GameId) -> Option<&GameConfig> {
        self.games.get(&game_id)
    }
}

/// Game identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameId {
    Arknights,
    Endfield,
}

impl GameId {
    /// Get the Unity streaming assets subdirectory name for this game.
    ///
    /// VFS files live under `{install_path}/{subdir}/StreamingAssets/`.
    pub fn streaming_assets_subdir(&self) -> &'static str {
        match self {
            GameId::Arknights => "Arknights_Data",
            GameId::Endfield => "Endfield_Data",
        }
    }
}

impl std::fmt::Display for GameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameId::Arknights => write!(f, "arknights"),
            GameId::Endfield => write!(f, "endfield"),
        }
    }
}

impl std::str::FromStr for GameId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "arknights" => Ok(GameId::Arknights),
            "endfield" => Ok(GameId::Endfield),
            _ => Err(anyhow::anyhow!("Unknown game: {}", s)),
        }
    }
}

/// Per-game configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameConfig {
    /// Installation path
    pub install_path: Option<PathBuf>,

    /// Currently active server
    #[serde(default)]
    pub active_server: ServerId,

    /// Tracked version for this install
    pub version: Option<String>,

    /// Last update timestamp
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,

    /// Per-server configurations
    #[serde(default)]
    pub servers: HashMap<ServerId, ServerConfig>,
}

impl GameConfig {
    /// Get the install path for a specific server
    ///
    /// Checks the server's own install_path first, then falls back to the
    /// game-level install_path for backward compatibility.
    pub fn server_install_path(&self, server: ServerId) -> Option<PathBuf> {
        self.servers
            .get(&server)
            .and_then(|s| s.install_path.clone())
            .or_else(|| self.install_path.clone())
    }
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            install_path: None,
            active_server: ServerId::CnOfficial,
            version: None,
            last_update: None,
            servers: HashMap::new(),
        }
    }
}

/// Server identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ServerId {
    #[default]
    CnOfficial,
    CnBilibili,
    GlobalOfficial,
    GlobalEpic,
    GlobalGoogleplay,
}

impl std::fmt::Display for ServerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerId::CnOfficial => write!(f, "cn_official"),
            ServerId::CnBilibili => write!(f, "cn_bilibili"),
            ServerId::GlobalOfficial => write!(f, "global_official"),
            ServerId::GlobalEpic => write!(f, "global_epic"),
            ServerId::GlobalGoogleplay => write!(f, "global_googleplay"),
        }
    }
}

impl std::str::FromStr for ServerId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "cn_official" => Ok(ServerId::CnOfficial),
            "cn_bilibili" => Ok(ServerId::CnBilibili),
            "global_official" => Ok(ServerId::GlobalOfficial),
            "global_epic" => Ok(ServerId::GlobalEpic),
            "global_googleplay" => Ok(ServerId::GlobalGoogleplay),
            _ => Err(anyhow::anyhow!("Unknown server: {}", s)),
        }
    }
}

impl ServerId {
    /// Get available servers for a game
    pub fn available_for(game: GameId) -> &'static [ServerId] {
        match game {
            GameId::Arknights => &[ServerId::CnOfficial, ServerId::CnBilibili],
            GameId::Endfield => &[
                ServerId::CnOfficial,
                ServerId::CnBilibili,
                ServerId::GlobalOfficial,
                ServerId::GlobalEpic,
                ServerId::GlobalGoogleplay,
            ],
        }
    }

    /// Get the default server for a game
    pub fn default_for(game: GameId) -> ServerId {
        match game {
            GameId::Arknights => ServerId::CnOfficial,
            GameId::Endfield => ServerId::CnOfficial,
        }
    }
}

/// Per-server configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    /// Whether this server has been downloaded
    pub installed: bool,

    /// Installation path for this server
    ///
    /// Each server can be installed at a different path (e.g. CN, Global, Bili
    /// in separate directories on the same machine for file reuse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_path: Option<PathBuf>,

    /// Version installed for this server
    pub version: Option<String>,

    /// Last update timestamp for this server
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,
}

/// Default application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultSettings {
    /// Default download directory
    pub download_path: Option<PathBuf>,

    /// Number of concurrent download connections
    #[serde(default = "default_concurrent_connections")]
    pub concurrent_connections: u32,

    /// Retry attempts for failed downloads
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: u32,
}

impl Default for DefaultSettings {
    fn default() -> Self {
        Self {
            download_path: None,
            concurrent_connections: default_concurrent_connections(),
            retry_attempts: default_retry_attempts(),
        }
    }
}

fn default_concurrent_connections() -> u32 {
    4
}

fn default_retry_attempts() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[compio::test]
    async fn test_config_save_load() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test_config.json");

        let mut config = Config::default();
        let game_id = GameId::Endfield;
        let game = config.game_mut(game_id);
        game.install_path = Some(PathBuf::from("C:\\Games\\Endfield"));
        game.version = Some("1.1.9".to_string());

        config.save_to(&config_path).await.unwrap();
        let loaded = Config::load_from(&config_path).await.unwrap();

        assert_eq!(loaded.games.len(), 1);
        assert_eq!(
            loaded.game(game_id).unwrap().version,
            Some("1.1.9".to_string())
        );
    }

    #[compio::test]
    async fn test_config_persistence() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("persistent_config.json");

        // Create and save config
        let mut config = Config::default();
        let game_id = GameId::Arknights;
        let game = config.game_mut(game_id);
        game.install_path = Some(PathBuf::from("C:\\Games\\Arknights"));
        game.active_server = ServerId::CnBilibili;
        game.version = Some("72.0.0".to_string());

        // Add server config
        let server = game.servers.entry(ServerId::CnBilibili).or_default();
        server.installed = true;
        server.version = Some("72.0.0".to_string());

        config.save_to(&config_path).await.unwrap();

        // Load and verify
        let loaded = Config::load_from(&config_path).await.unwrap();
        let loaded_game = loaded.game(game_id).unwrap();

        assert_eq!(
            loaded_game.install_path,
            Some(PathBuf::from("C:\\Games\\Arknights"))
        );
        assert_eq!(loaded_game.active_server, ServerId::CnBilibili);
        assert_eq!(loaded_game.version, Some("72.0.0".to_string()));
        assert!(
            loaded_game
                .servers
                .get(&ServerId::CnBilibili)
                .unwrap()
                .installed
        );
    }

    #[test]
    fn test_config_migration() {
        let old_config_json = r#"{
            "version": 0,
            "games": {},
            "accounts": [],
            "defaults": {
                "concurrent_connections": 4,
                "retry_attempts": 3
            }
        }"#;
        let config: Config = serde_json::from_str(old_config_json).unwrap();
        let migrated = config.migrate().unwrap();
        assert_eq!(migrated.version, CONFIG_VERSION);
    }

    #[test]
    fn test_game_id_parsing() {
        assert_eq!("arknights".parse::<GameId>().unwrap(), GameId::Arknights);
        assert_eq!("endfield".parse::<GameId>().unwrap(), GameId::Endfield);
        assert_eq!("ARKNIGHTS".parse::<GameId>().unwrap(), GameId::Arknights); // Case insensitive
        assert_eq!("EndField".parse::<GameId>().unwrap(), GameId::Endfield);
        assert!("unknown".parse::<GameId>().is_err());
        assert!("".parse::<GameId>().is_err());
    }

    #[test]
    fn test_game_id_display() {
        assert_eq!(GameId::Arknights.to_string(), "arknights");
        assert_eq!(GameId::Endfield.to_string(), "endfield");
    }

    #[test]
    fn test_server_id_parsing() {
        assert_eq!(
            "cn_official".parse::<ServerId>().unwrap(),
            ServerId::CnOfficial
        );
        assert_eq!(
            "cn_bilibili".parse::<ServerId>().unwrap(),
            ServerId::CnBilibili
        );
        assert_eq!(
            "global_official".parse::<ServerId>().unwrap(),
            ServerId::GlobalOfficial
        );
        assert_eq!(
            "global_epic".parse::<ServerId>().unwrap(),
            ServerId::GlobalEpic
        );
        assert_eq!(
            "global_googleplay".parse::<ServerId>().unwrap(),
            ServerId::GlobalGoogleplay
        );
        // Case insensitive
        assert_eq!(
            "CN_OFFICIAL".parse::<ServerId>().unwrap(),
            ServerId::CnOfficial
        );
        assert!("unknown".parse::<ServerId>().is_err());
    }

    #[test]
    fn test_server_id_display() {
        assert_eq!(ServerId::CnOfficial.to_string(), "cn_official");
        assert_eq!(ServerId::CnBilibili.to_string(), "cn_bilibili");
        assert_eq!(ServerId::GlobalOfficial.to_string(), "global_official");
        assert_eq!(ServerId::GlobalEpic.to_string(), "global_epic");
        assert_eq!(ServerId::GlobalGoogleplay.to_string(), "global_googleplay");
    }

    #[test]
    fn test_server_id_default_for() {
        assert_eq!(
            ServerId::default_for(GameId::Arknights),
            ServerId::CnOfficial
        );
        assert_eq!(
            ServerId::default_for(GameId::Endfield),
            ServerId::CnOfficial
        );
    }

    #[test]
    fn test_server_id_available_for() {
        let ark_servers = ServerId::available_for(GameId::Arknights);
        assert_eq!(ark_servers.len(), 2);
        assert!(ark_servers.contains(&ServerId::CnOfficial));
        assert!(ark_servers.contains(&ServerId::CnBilibili));

        let ef_servers = ServerId::available_for(GameId::Endfield);
        assert_eq!(ef_servers.len(), 5);
        assert!(ef_servers.contains(&ServerId::GlobalEpic));
        assert!(ef_servers.contains(&ServerId::GlobalGoogleplay));
    }

    #[test]
    fn test_default_settings() {
        let defaults = DefaultSettings::default();
        assert_eq!(defaults.concurrent_connections, 4);
        assert_eq!(defaults.retry_attempts, 3);
        assert!(defaults.download_path.is_none());
    }

    #[test]
    fn test_game_config_default() {
        let config = GameConfig::default();
        assert!(config.install_path.is_none());
        assert_eq!(config.active_server, ServerId::CnOfficial);
        assert!(config.version.is_none());
        assert!(config.last_update.is_none());
        assert!(config.servers.is_empty());
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.version, CONFIG_VERSION);
        assert!(config.games.is_empty());
        assert_eq!(config.defaults.concurrent_connections, 4);
    }

    #[compio::test]
    async fn test_config_nonexistent_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("nonexistent.json");

        // Loading from non-existent file should return default config
        let config = Config::load_from(&config_path).await.unwrap();
        assert_eq!(config.games.len(), 0);
        assert_eq!(config.version, CONFIG_VERSION);
    }

    #[test]
    fn test_server_config_default() {
        let server = ServerConfig::default();
        assert!(!server.installed);
        assert!(server.install_path.is_none());
        assert!(server.version.is_none());
        assert!(server.last_update.is_none());
    }

    #[test]
    fn test_server_install_path_prefers_own() {
        let mut config = GameConfig::default();
        config.install_path = Some(PathBuf::from("C:\\Games\\Shared"));
        let server = config.servers.entry(ServerId::CnBilibili).or_default();
        server.install_path = Some(PathBuf::from("C:\\Games\\Bili"));
        server.installed = true;

        // Server-specific path takes priority
        assert_eq!(
            config.server_install_path(ServerId::CnBilibili),
            Some(PathBuf::from("C:\\Games\\Bili"))
        );
        // Fallback to game-level path for unconfigured server
        assert_eq!(
            config.server_install_path(ServerId::GlobalOfficial),
            Some(PathBuf::from("C:\\Games\\Shared"))
        );
    }

    #[test]
    fn test_migration_moves_install_path_to_server() {
        let json = r#"{
            "version": 0,
            "games": {
                "endfield": {
                    "install_path": "C:\\Games\\Endfield",
                    "active_server": "cn_official",
                    "version": "1.1.9",
                    "servers": {
                        "cn_official": { "installed": true, "version": "1.1.9" }
                    }
                }
            },
            "defaults": { "concurrent_connections": 4, "retry_attempts": 3 }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        let migrated = config.migrate().unwrap();

        let game = migrated.game(GameId::Endfield).unwrap();
        assert_eq!(
            game.install_path,
            Some(PathBuf::from("C:\\Games\\Endfield"))
        );
        let server = game.servers.get(&ServerId::CnOfficial).unwrap();
        assert_eq!(
            server.install_path,
            Some(PathBuf::from("C:\\Games\\Endfield"))
        );
    }

    #[test]
    fn test_migration_copies_path_to_all_installed_servers() {
        // Two installed servers sharing one path — migration should copy to both
        let json = r#"{
            "version": 0,
            "games": {
                "endfield": {
                    "install_path": "C:\\Games\\Endfield",
                    "active_server": "cn_official",
                    "version": "1.1.9",
                    "servers": {
                        "cn_official": { "installed": true, "version": "1.1.9" },
                        "cn_bilibili": { "installed": true, "version": "1.1.9" }
                    }
                }
            },
            "defaults": { "concurrent_connections": 4, "retry_attempts": 3 }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        let migrated = config.migrate().unwrap();

        let game = migrated.game(GameId::Endfield).unwrap();
        // Both servers should have the shared path
        let cn = game.servers.get(&ServerId::CnOfficial).unwrap();
        assert_eq!(cn.install_path, Some(PathBuf::from("C:\\Games\\Endfield")));
        let bili = game.servers.get(&ServerId::CnBilibili).unwrap();
        assert_eq!(
            bili.install_path,
            Some(PathBuf::from("C:\\Games\\Endfield"))
        );
        // Game-level install_path should be preserved
        assert_eq!(
            game.install_path,
            Some(PathBuf::from("C:\\Games\\Endfield"))
        );
    }
}

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::api::crypto;
use griffr_common::config::{GameConfig, GameId, ServerId};
use griffr_common::game::GameManager;

#[derive(Debug, Clone)]
pub struct ParsedConfigIni {
    pub path: PathBuf,
    pub raw: String,
    pub fields: BTreeMap<String, String>,
}

impl ParsedConfigIni {
    pub fn version(&self) -> Option<&str> {
        self.fields.get("version").map(String::as_str)
    }

    pub fn entry(&self) -> Option<&str> {
        self.fields.get("entry").map(String::as_str)
    }

    pub fn appcode(&self) -> Option<&str> {
        self.fields.get("appcode").map(String::as_str)
    }

    pub fn region(&self) -> Option<&str> {
        self.fields.get("region").map(String::as_str)
    }

    pub fn channel(&self) -> Option<&str> {
        self.fields.get("channel").map(String::as_str)
    }

    pub fn sub_channel(&self) -> Option<&str> {
        self.fields.get("sub_channel").map(String::as_str)
    }
}

#[derive(Debug, Clone)]
pub struct LocalInstall {
    pub install_path: PathBuf,
    pub config_ini: ParsedConfigIni,
    pub game_id: Option<GameId>,
    pub server_id: Option<ServerId>,
}

impl LocalInstall {
    pub fn require_known_game(&self) -> Result<GameId> {
        self.game_id.context(format!(
            "Could not map local install to a supported game from {}",
            self.install_path.display()
        ))
    }

    pub fn require_known_server(&self) -> Result<ServerId> {
        self.server_id.context(format!(
            "Could not map local install to a supported server from {}",
            self.install_path.display()
        ))
    }

    /// Resolve installed game version from decrypted `config.ini`.
    ///
    /// `config.ini` is the only supported version source of truth for local
    /// installs, because it is launcher-managed metadata shipped with official
    /// game files.
    pub fn require_config_ini_version(&self) -> Result<&str> {
        self.config_ini.version().context(format!(
            "config.ini at {} does not contain a version field",
            self.config_ini.path.display()
        ))
    }

    pub fn as_game_config(&self) -> Result<GameConfig> {
        let game_id = self.require_known_game()?;
        let server_id = self.require_known_server()?;
        let version = self.require_config_ini_version()?.to_string();

        let mut config = GameConfig {
            install_path: Some(self.install_path.clone()),
            active_server: server_id,
            version: Some(version.clone()),
            last_update: None,
            servers: Default::default(),
        };
        let server = config.servers.entry(server_id).or_default();
        server.installed = true;
        server.install_path = Some(self.install_path.clone());
        server.version = Some(version);

        let _ = game_id;
        Ok(config)
    }

    pub fn as_manager(&self) -> Result<GameManager> {
        Ok(GameManager::new(
            self.require_known_game()?,
            self.as_game_config()?,
        ))
    }
}

pub fn resolve_install_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    }
}

pub fn resolve_named_path(path: &Path, filename: &str) -> PathBuf {
    if path.is_dir() {
        path.join(filename)
    } else {
        path.to_path_buf()
    }
}

pub async fn decrypt_config_ini(path: &Path) -> Result<ParsedConfigIni> {
    let config_path = resolve_named_path(path, "config.ini");
    let encrypted = compio::fs::read(&config_path)
        .await
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let raw = crypto::decrypt_game_files(&encrypted)
        .with_context(|| format!("Failed to decrypt {}", config_path.display()))?;

    let mut fields = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    Ok(ParsedConfigIni {
        path: config_path,
        raw,
        fields,
    })
}

pub async fn detect_local_install(path: &Path) -> Result<LocalInstall> {
    let install_path = resolve_install_path(path);
    let config_ini = decrypt_config_ini(&install_path).await?;

    let has_arknights_exe = match compio::fs::metadata(install_path.join("Arknights.exe")).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to stat {}",
                    install_path.join("Arknights.exe").display()
                )
            })
        }
    };
    let has_endfield_exe = match compio::fs::metadata(install_path.join("Endfield.exe")).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to stat {}",
                    install_path.join("Endfield.exe").display()
                )
            })
        }
    };

    let game_id = detect_game_id(&config_ini, has_arknights_exe, has_endfield_exe);
    let server_id = detect_server_id(&config_ini);

    Ok(LocalInstall {
        install_path,
        config_ini,
        game_id,
        server_id,
    })
}

fn detect_game_id(
    config_ini: &ParsedConfigIni,
    has_arknights_exe: bool,
    has_endfield_exe: bool,
) -> Option<GameId> {
    match config_ini.appcode() {
        Some("GzD1CpaWgmSq1wew") => return Some(GameId::Arknights),
        Some("6LL0KJuqHBVz33WK") | Some("YDUTE5gscDZ229CW") => return Some(GameId::Endfield),
        _ => {}
    }

    match config_ini.entry() {
        Some(entry) if entry.eq_ignore_ascii_case("Arknights.exe") => Some(GameId::Arknights),
        Some(entry) if entry.eq_ignore_ascii_case("Endfield.exe") => Some(GameId::Endfield),
        _ => {
            if has_arknights_exe {
                Some(GameId::Arknights)
            } else if has_endfield_exe {
                Some(GameId::Endfield)
            } else {
                None
            }
        }
    }
}

fn detect_server_id(config_ini: &ParsedConfigIni) -> Option<ServerId> {
    match (config_ini.channel(), config_ini.sub_channel()) {
        (Some("1"), Some("1")) => Some(ServerId::CnOfficial),
        (Some("1"), Some("2")) => Some(ServerId::CnBilibili),
        (Some("2"), Some("2")) => Some(ServerId::CnBilibili),
        (Some("6"), Some("6")) => Some(ServerId::GlobalOfficial),
        (Some("6"), Some("801")) => Some(ServerId::GlobalEpic),
        (Some("6"), Some("802")) => Some(ServerId::GlobalGoogleplay),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_with_channel(channel: &str, sub_channel: &str) -> ParsedConfigIni {
        let mut fields = BTreeMap::new();
        fields.insert("channel".to_string(), channel.to_string());
        fields.insert("sub_channel".to_string(), sub_channel.to_string());
        ParsedConfigIni {
            path: PathBuf::from("config.ini"),
            raw: String::new(),
            fields,
        }
    }

    #[test]
    fn detect_server_maps_cn_bilibili_legacy_tuple() {
        let parsed = parsed_with_channel("1", "2");
        assert_eq!(detect_server_id(&parsed), Some(ServerId::CnBilibili));
    }

    #[test]
    fn detect_server_maps_cn_bilibili_new_tuple() {
        let parsed = parsed_with_channel("2", "2");
        assert_eq!(detect_server_id(&parsed), Some(ServerId::CnBilibili));
    }

    #[test]
    fn detect_server_maps_global_epic_tuple() {
        let parsed = parsed_with_channel("6", "801");
        assert_eq!(detect_server_id(&parsed), Some(ServerId::GlobalEpic));
    }

    #[test]
    fn detect_server_maps_global_googleplay_tuple() {
        let parsed = parsed_with_channel("6", "802");
        assert_eq!(detect_server_id(&parsed), Some(ServerId::GlobalGoogleplay));
    }

    #[test]
    fn require_config_ini_version_returns_version() {
        let mut fields = BTreeMap::new();
        fields.insert("version".to_string(), "1.1.9".to_string());

        let local = LocalInstall {
            install_path: PathBuf::from("C:\\Games\\Endfield"),
            config_ini: ParsedConfigIni {
                path: PathBuf::from("config.ini"),
                raw: "version=1.1.9".to_string(),
                fields,
            },
            game_id: Some(GameId::Endfield),
            server_id: Some(ServerId::CnOfficial),
        };

        assert_eq!(local.require_config_ini_version().unwrap(), "1.1.9");
    }

    #[test]
    fn require_config_ini_version_errors_when_missing() {
        let local = LocalInstall {
            install_path: PathBuf::from("C:\\Games\\Endfield"),
            config_ini: ParsedConfigIni {
                path: PathBuf::from("config.ini"),
                raw: String::new(),
                fields: BTreeMap::new(),
            },
            game_id: Some(GameId::Endfield),
            server_id: Some(ServerId::CnOfficial),
        };

        let err = local.require_config_ini_version().unwrap_err();
        assert!(err.to_string().contains("config.ini"));
        assert!(err.to_string().contains("version field"));
    }
}

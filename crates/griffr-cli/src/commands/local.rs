use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::api::crypto;
use griffr_common::config::{
    game_by_appcode, game_by_executable, ChannelPair, GameId, GAME_CATALOG,
};

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
    pub channel_id: Option<ChannelPair>,
}

impl LocalInstall {
    pub fn require_known_game(&self) -> Result<GameId> {
        self.game_id.clone().context(format!(
            "Could not map local install to a supported game from {}",
            self.install_path.display()
        ))
    }

    pub fn require_known_channel(&self) -> Result<ChannelPair> {
        self.channel_id.clone().context(format!(
            "Could not map local install to a supported channel from {}",
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

    let mut games_with_existing_executable = Vec::new();
    for game in GAME_CATALOG {
        let executable = install_path.join(game.executable);
        match compio::fs::metadata(&executable).await {
            Ok(_) => games_with_existing_executable.push(game.game_id()),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to stat {}", executable.display()))
            }
        }
    }

    let game_id = detect_game_id(&config_ini, &games_with_existing_executable);
    let channel_id = detect_channel_id(&config_ini);

    Ok(LocalInstall {
        install_path,
        config_ini,
        game_id,
        channel_id,
    })
}

fn detect_game_id(
    config_ini: &ParsedConfigIni,
    games_with_existing_executable: &[GameId],
) -> Option<GameId> {
    config_ini
        .appcode()
        .and_then(game_by_appcode)
        .or_else(|| config_ini.entry().and_then(game_by_executable))
        .or_else(|| games_with_existing_executable.first().cloned())
}

fn detect_channel_id(config_ini: &ParsedConfigIni) -> Option<ChannelPair> {
    let channel = config_ini.channel()?;
    ChannelPair::parse(channel, config_ini.sub_channel()).ok()
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
    fn detect_channel_preserves_independent_pair() {
        let parsed = parsed_with_channel("1", "802");
        let channel = detect_channel_id(&parsed).unwrap();
        assert_eq!(channel.channel().as_str(), "1");
        assert_eq!(channel.sub_channel().as_str(), "802");
    }

    #[test]
    fn detect_channel_preserves_unknown_server_validated_values() {
        let parsed = parsed_with_channel("123", "456");
        let channel = detect_channel_id(&parsed).unwrap();
        assert_eq!(channel.channel().as_str(), "123");
        assert_eq!(channel.sub_channel().as_str(), "456");
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
            game_id: Some(GameId::ENDFIELD),
            channel_id: Some(ChannelPair::parse("1", None::<String>).unwrap()),
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
            game_id: Some(GameId::ENDFIELD),
            channel_id: Some(ChannelPair::parse("1", None::<String>).unwrap()),
        };

        let err = local.require_config_ini_version().unwrap_err();
        assert!(err.to_string().contains("config.ini"));
        assert!(err.to_string().contains("version field"));
    }
}

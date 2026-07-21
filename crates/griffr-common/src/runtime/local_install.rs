use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::api::crypto;
use crate::config::{
    game_by_appcode, game_by_exe_name, ChannelPair, GameId, RegionId, GAME_DEFINITIONS,
};
use crate::error::{Error, Result};
use crate::runtime::CONFIG_INI_NAME;

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
    pub region_id: Option<RegionId>,
    pub channel_id: Option<ChannelPair>,
}

impl LocalInstall {
    pub fn require_known_game(&self) -> Result<GameId> {
        self.game_id.clone().ok_or_else(|| Error::Message {
            context: "Configuration error: ",
            detail: format!(
                "Could not map local install to a supported game from {}",
                self.install_path.display()
            ),
        })
    }

    pub fn require_known_region(&self) -> Result<RegionId> {
        self.region_id.ok_or_else(|| Error::Message {
            context: "Configuration error: ",
            detail: format!(
                "Could not map local install to a supported region from {}",
                self.install_path.display()
            ),
        })
    }

    pub fn require_known_channel(&self) -> Result<ChannelPair> {
        self.channel_id.clone().ok_or_else(|| Error::Message {
            context: "Configuration error: ",
            detail: format!(
                "Could not map local install to a supported channel from {}",
                self.install_path.display()
            ),
        })
    }

    /// Resolve installed game version from decrypted `config.ini`.
    ///
    /// `config.ini` is the launcher-managed source of truth for installed
    /// versions and is shared by CLI and GUI consumers.
    pub fn require_config_ini_version(&self) -> Result<&str> {
        self.config_ini.version().ok_or_else(|| Error::Message {
            context: "Configuration error: ",
            detail: format!(
                "config.ini at {} does not contain a version field",
                self.config_ini.path.display()
            ),
        })
    }
}

use super::path_is_dir;

pub async fn resolve_install_path(path: &Path) -> PathBuf {
    if path_is_dir(path).await {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    }
}

pub async fn resolve_named_path(path: &Path, filename: &str) -> PathBuf {
    if path_is_dir(path).await {
        path.join(filename)
    } else {
        path.to_path_buf()
    }
}

pub async fn decrypt_config_ini(path: &Path) -> Result<ParsedConfigIni> {
    let config_path = resolve_named_path(path, CONFIG_INI_NAME).await;
    let encrypted = compio::fs::read(&config_path)
        .await
        .map_err(|source| Error::IoAt {
            action: "open file",
            path: config_path.clone(),
            source,
        })?;
    let raw = crypto::decrypt_game_files(&encrypted).map_err(|error| Error::Message {
        context: "Crypto error: ",
        detail: format!("Failed to decrypt {}: {error}", config_path.display()),
    })?;

    let fields = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('['))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect();

    Ok(ParsedConfigIni {
        path: config_path,
        raw,
        fields,
    })
}

pub async fn detect_local_install(path: &Path) -> Result<LocalInstall> {
    let install_path = resolve_install_path(path).await;
    let config_ini = decrypt_config_ini(&install_path).await?;

    let mut games_with_existing_exe = Vec::new();
    for game in GAME_DEFINITIONS {
        let exe_path = install_path.join(game.exe_name);
        match compio::fs::metadata(&exe_path).await {
            Ok(_) => games_with_existing_exe.push(game.game_id()),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::IoAt {
                    action: "query file metadata/stat for",
                    path: exe_path,
                    source,
                })
            }
        }
    }

    let game_id = detect_game_id(&config_ini, &games_with_existing_exe);
    let region_id = detect_region_id(&config_ini);
    let channel_id = detect_channel_id(&config_ini);

    Ok(LocalInstall {
        install_path,
        config_ini,
        game_id,
        region_id,
        channel_id,
    })
}

fn detect_game_id(
    config_ini: &ParsedConfigIni,
    games_with_existing_exe: &[GameId],
) -> Option<GameId> {
    config_ini
        .appcode()
        .and_then(game_by_appcode)
        .or_else(|| config_ini.entry().and_then(game_by_exe_name))
        .or_else(|| games_with_existing_exe.first().cloned())
}

fn detect_region_id(config_ini: &ParsedConfigIni) -> Option<RegionId> {
    config_ini.region()?.parse().ok()
}

fn detect_channel_id(config_ini: &ParsedConfigIni) -> Option<ChannelPair> {
    let channel = config_ini.channel()?;
    ChannelPair::from_api(channel, config_ini.sub_channel()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_with_channel(channel: &str, sub_channel: &str) -> ParsedConfigIni {
        let fields = BTreeMap::from([
            ("channel".to_string(), channel.to_string()),
            ("sub_channel".to_string(), sub_channel.to_string()),
        ]);
        ParsedConfigIni {
            path: PathBuf::from("config.ini"),
            raw: String::new(),
            fields,
        }
    }

    #[test]
    fn detect_channel_preserves_independent_pair() {
        let channel = detect_channel_id(&parsed_with_channel("1", "802")).unwrap();
        assert_eq!(channel.channel().as_str(), "1");
        assert_eq!(channel.sub_channel().as_str(), "802");
    }

    #[test]
    fn detect_channel_preserves_unknown_server_validated_values() {
        let channel = detect_channel_id(&parsed_with_channel("123", "456")).unwrap();
        assert_eq!(channel.channel().as_str(), "123");
        assert_eq!(channel.sub_channel().as_str(), "456");
    }

    #[test]
    fn require_config_ini_version_returns_version() {
        let local = LocalInstall {
            install_path: PathBuf::from(r"C:\Games\Endfield"),
            config_ini: ParsedConfigIni {
                path: PathBuf::from("config.ini"),
                raw: "version=1.1.9".to_string(),
                fields: BTreeMap::from([("version".to_string(), "1.1.9".to_string())]),
            },
            game_id: Some(GameId::ENDFIELD),
            region_id: Some(RegionId::Cn),
            channel_id: Some(ChannelPair::from_api("1", None::<String>).unwrap()),
        };

        assert_eq!(local.require_config_ini_version().unwrap(), "1.1.9");
    }

    #[test]
    fn require_config_ini_version_errors_when_missing() {
        let local = LocalInstall {
            install_path: PathBuf::from(r"C:\Games\Endfield"),
            config_ini: ParsedConfigIni {
                path: PathBuf::from("config.ini"),
                raw: String::new(),
                fields: BTreeMap::new(),
            },
            game_id: Some(GameId::ENDFIELD),
            region_id: Some(RegionId::Cn),
            channel_id: Some(ChannelPair::from_api("1", None::<String>).unwrap()),
        };

        let err = local.require_config_ini_version().unwrap_err();
        assert!(err.to_string().contains("config.ini"));
        assert!(err.to_string().contains("version field"));
    }
}

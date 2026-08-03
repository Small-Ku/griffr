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

fn filename_matches(path: &Path, filename: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(filename)
            } else {
                name == filename
            }
        })
}

/// Resolve an existing install root or an explicitly named `config.ini` path.
///
/// A missing ordinary path remains an install-root candidate. It must never be
/// silently replaced by its parent, because destructive callers such as
/// uninstall would otherwise target the wrong directory.
pub async fn resolve_install_path(path: &Path) -> Result<PathBuf> {
    match compio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(path.to_path_buf()),
        Ok(metadata) if metadata.is_file() => {
            if !filename_matches(path, CONFIG_INI_NAME) {
                return Err(Error::Message {
                    context: "Path error: ",
                    detail: format!(
                        "Expected an install directory or {}, found file {}",
                        CONFIG_INI_NAME,
                        path.display()
                    ),
                });
            }
            path.parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| Error::Message {
                    context: "Path error: ",
                    detail: format!("{} has no parent install directory", path.display()),
                })
        }
        Ok(_) => Err(Error::Message {
            context: "Path error: ",
            detail: format!(
                "Expected an install directory or regular {}, found {}",
                CONFIG_INI_NAME,
                path.display()
            ),
        }),
        Err(source) if source.kind() == ErrorKind::NotFound => {
            if filename_matches(path, CONFIG_INI_NAME) {
                path.parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| Error::Message {
                        context: "Path error: ",
                        detail: format!("{} has no parent install directory", path.display()),
                    })
            } else {
                Ok(path.to_path_buf())
            }
        }
        Err(source) => Err(Error::IoAt {
            action: "query file metadata/stat for",
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub async fn resolve_named_path(path: &Path, filename: &str) -> Result<PathBuf> {
    match compio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(path.join(filename)),
        Ok(metadata) if metadata.is_file() => Ok(path.to_path_buf()),
        Ok(_) => Err(Error::Message {
            context: "Path error: ",
            detail: format!(
                "Expected a directory or regular file, found {}",
                path.display()
            ),
        }),
        Err(source) if source.kind() == ErrorKind::NotFound => {
            if filename_matches(path, filename) {
                Ok(path.to_path_buf())
            } else {
                Ok(path.join(filename))
            }
        }
        Err(source) => Err(Error::IoAt {
            action: "query file metadata/stat for",
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub async fn decrypt_config_ini(path: &Path) -> Result<ParsedConfigIni> {
    let config_path = resolve_named_path(path, CONFIG_INI_NAME).await?;
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
    let install_path = resolve_install_path(path).await?;
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

    #[compio::test]
    async fn missing_install_root_is_not_replaced_by_parent() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-install");

        assert_eq!(resolve_install_path(&missing).await.unwrap(), missing);
        assert_eq!(
            resolve_named_path(&missing, CONFIG_INI_NAME).await.unwrap(),
            missing.join(CONFIG_INI_NAME)
        );
    }

    #[compio::test]
    async fn explicit_config_path_resolves_to_parent_even_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(CONFIG_INI_NAME);

        assert_eq!(
            resolve_install_path(&config).await.unwrap(),
            temp.path().to_path_buf()
        );
        assert_eq!(
            resolve_named_path(&config, CONFIG_INI_NAME).await.unwrap(),
            config
        );
    }

    #[compio::test]
    async fn unrelated_existing_file_is_rejected_as_install_input() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("notes.txt");
        std::fs::write(&file, b"not an install").unwrap();

        let error = resolve_install_path(&file).await.unwrap_err();
        assert!(error.to_string().contains("Expected an install directory"));
    }
}

//! Channel/channel management for game switching

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use crate::config::{resolve_install_profile, ChannelId, GameId};
#[cfg(not(windows))]
use crate::runtime::read_link;
use crate::runtime::{dir_size, directory_has_entries};

/// Channel directory management
#[derive(Debug)]
pub struct Channel {
    game_id: GameId,
    channel_id: ChannelId,
    base_path: PathBuf,
}

impl Channel {
    async fn path_exists(path: &Path) -> bool {
        match compio::fs::metadata(path).await {
            Ok(_) => true,
            Err(err) if err.kind() == ErrorKind::NotFound => false,
            Err(_) => false,
        }
    }

    /// Create a new channel instance
    pub fn new(game_id: GameId, channel_id: ChannelId, base_path: impl Into<PathBuf>) -> Self {
        Self {
            game_id,
            channel_id,
            base_path: base_path.into(),
        }
    }

    /// Get the channel ID
    pub fn channel_id(&self) -> ChannelId {
        self.channel_id.clone()
    }

    /// Get the game ID
    pub fn game_id(&self) -> GameId {
        self.game_id.clone()
    }

    /// Get the channel-specific directory path
    pub fn channel_path(&self) -> PathBuf {
        self.base_path
            .join("channels")
            .join(format!("{}", self.channel_id))
    }

    /// Get the "active" symlink/junction path
    pub fn active_path(&self) -> PathBuf {
        self.base_path.join("active")
    }

    /// Check if this channel is installed (directory exists and has files)
    pub async fn is_installed(&self) -> bool {
        let path = self.channel_path();
        if !Self::path_exists(&path).await {
            return false;
        }

        directory_has_entries(path).await.unwrap_or(false)
    }

    /// Check if this channel is currently active (symlink points to it)
    #[cfg(windows)]
    pub async fn is_active(&self) -> bool {
        use std::os::windows::fs::MetadataExt;

        let active_path = self.active_path();
        if !Self::path_exists(&active_path).await {
            return false;
        }

        let expected_target = self.channel_path();
        compio::runtime::spawn_blocking(move || match std::fs::metadata(&active_path) {
            Ok(metadata) => {
                let file_attributes = metadata.file_attributes();
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    if let Ok(target) = std::fs::read_link(&active_path) {
                        target == expected_target
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Err(_) => false,
        })
        .await
        .unwrap_or(false)
    }

    #[cfg(not(windows))]
    pub async fn is_active(&self) -> bool {
        let active_path = self.active_path();
        if !Self::path_exists(&active_path).await {
            return false;
        }

        let expected_target = self.channel_path();
        read_link(active_path)
            .await
            .map(|target| target == expected_target)
            .unwrap_or(false)
    }

    /// Activate this channel by creating a symlink/junction
    pub async fn activate(&self) -> Result<()> {
        let channel_path = self.channel_path();
        let active_path = self.active_path();

        // Ensure channel directory exists
        compio::fs::create_dir_all(&channel_path)
            .await
            .map_err(|e| Error::CreateDirFailed {
                path: channel_path.clone(),
                source: e,
            })?;

        // Remove existing active junction/symlink if it exists
        if Self::path_exists(&active_path).await {
            #[cfg(windows)]
            {
                // On Windows, we need to use std::fs for removing junctions
                compio::fs::remove_dir(&active_path)
                    .await
                    .map_err(|e| Error::RemoveFailed {
                        path: active_path.clone(),
                        source: e,
                    })?;
            }
            #[cfg(not(windows))]
            {
                compio::fs::remove_file(&active_path).await?;
            }
        }

        // Create new junction/symlink
        #[cfg(windows)]
        {
            self.create_junction(&active_path, &channel_path)?;
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(&channel_path, &active_path)?;
        }

        Ok(())
    }

    /// Create a Windows junction point
    #[cfg(windows)]
    fn create_junction(&self, junction: &Path, target: &Path) -> Result<()> {
        let junction_str = junction.to_string_lossy();
        let target_str = target.to_string_lossy();

        let status = std::process::Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/j",
                &format!("\"{}\"", junction_str),
                &format!("\"{}\"", target_str),
            ])
            .status()
            .map_err(|e| Error::Server(format!("Failed to execute mklink command: {e}")))?;

        if !status.success() {
            return Err(Error::Server(format!(
                "mklink /j failed with status {}. Ensure the target directory exists and the junction path does not.",
                status
            )));
        }

        Ok(())
    }

    /// Get the path to a file within this channel
    pub fn file_path(&self, relative_path: impl AsRef<Path>) -> PathBuf {
        self.channel_path().join(relative_path)
    }

    /// Get the game executable path
    pub fn game_exe_path(&self) -> Result<PathBuf> {
        let profile =
            resolve_install_profile(&self.game_id, &self.channel_id, &Default::default())?;
        Ok(self.file_path(&profile.executable))
    }

    /// Calculate the total size of the channel installation
    pub async fn calculate_size(&self) -> Result<u64> {
        let channel_path = self.channel_path();
        if !Self::path_exists(&channel_path).await {
            return Ok(0);
        }

        dir_size(channel_path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_paths() {
        let channel = Channel::new(
            GameId::ENDFIELD,
            ChannelId::CN_OFFICIAL,
            PathBuf::from("C:\\Games\\Endfield"),
        );

        assert_eq!(
            channel.channel_path(),
            PathBuf::from("C:\\Games\\Endfield\\channels\\cn_official")
        );
        assert_eq!(
            channel.active_path(),
            PathBuf::from("C:\\Games\\Endfield\\active")
        );
        assert_eq!(
            channel.game_exe_path().unwrap(),
            PathBuf::from("C:\\Games\\Endfield\\channels\\cn_official\\Endfield.exe")
        );
    }
}

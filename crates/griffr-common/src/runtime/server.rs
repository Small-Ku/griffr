//! Server/channel management for game switching

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{GameId, ServerId};

/// Server directory management
#[derive(Debug)]
pub struct Server {
    game_id: GameId,
    server_id: ServerId,
    base_path: PathBuf,
}

impl Server {
    async fn path_exists(path: &Path) -> bool {
        match compio::fs::metadata(path).await {
            Ok(_) => true,
            Err(err) if err.kind() == ErrorKind::NotFound => false,
            Err(_) => false,
        }
    }

    /// Create a new server instance
    pub fn new(game_id: GameId, server_id: ServerId, base_path: impl Into<PathBuf>) -> Self {
        Self {
            game_id,
            server_id,
            base_path: base_path.into(),
        }
    }

    /// Get the server ID
    pub fn server_id(&self) -> ServerId {
        self.server_id
    }

    /// Get the game ID
    pub fn game_id(&self) -> GameId {
        self.game_id
    }

    /// Get the server-specific directory path
    pub fn server_path(&self) -> PathBuf {
        self.base_path
            .join("servers")
            .join(format!("{}", self.server_id))
    }

    /// Get the "active" symlink/junction path
    pub fn active_path(&self) -> PathBuf {
        self.base_path.join("active")
    }

    /// Check if this server is installed (directory exists and has files)
    pub async fn is_installed(&self) -> bool {
        let path = self.server_path();
        if !Self::path_exists(&path).await {
            return false;
        }

        // Check if the directory has any files
        match std::fs::read_dir(&path) {
            Ok(mut entries) => matches!(entries.next(), Some(Ok(_))),
            Err(_) => false,
        }
    }

    /// Check if this server is currently active (symlink points to it)
    #[cfg(windows)]
    pub async fn is_active(&self) -> bool {
        use std::os::windows::fs::MetadataExt;

        let active_path = self.active_path();
        if !Self::path_exists(&active_path).await {
            return false;
        }

        // On Windows, check if the active path is a junction/reparse point
        match std::fs::metadata(&active_path) {
            Ok(metadata) => {
                // Check if it's a reparse point (junction/symlink)
                let file_attributes = metadata.file_attributes();
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    // Try to read the junction target
                    if let Ok(target) = std::fs::read_link(&active_path) {
                        target == self.server_path()
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    #[cfg(not(windows))]
    pub async fn is_active(&self) -> bool {
        let active_path = self.active_path();
        if !Self::path_exists(&active_path).await {
            return false;
        }

        match std::fs::read_link(&active_path) {
            Ok(target) => target == self.server_path(),
            Err(_) => false,
        }
    }

    /// Activate this server by creating a symlink/junction
    pub async fn activate(&self) -> Result<()> {
        let server_path = self.server_path();
        let active_path = self.active_path();

        // Ensure server directory exists
        compio::fs::create_dir_all(&server_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to create server directory {}",
                    server_path.display()
                )
            })?;

        // Remove existing active junction/symlink if it exists
        if Self::path_exists(&active_path).await {
            #[cfg(windows)]
            {
                // On Windows, we need to use std::fs for removing junctions
                compio::fs::remove_dir(&active_path)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to remove existing active junction at {}",
                            active_path.display()
                        )
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
            self.create_junction(&active_path, &server_path)?;
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(&server_path, &active_path)?;
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
            .with_context(|| "Failed to execute mklink command")?;

        if !status.success() {
            anyhow::bail!(
                "mklink /j failed with status {}. Ensure the target directory exists and the junction path does not.",
                status
            );
        }

        Ok(())
    }

    /// Get the path to a file within this server
    pub fn file_path(&self, relative_path: impl AsRef<Path>) -> PathBuf {
        self.server_path().join(relative_path)
    }

    /// Get the game executable path
    pub fn game_exe_path(&self) -> PathBuf {
        let exe_name = match self.game_id {
            GameId::Arknights => "Arknights.exe",
            GameId::Endfield => "Endfield.exe",
        };
        self.file_path(exe_name)
    }

    /// Calculate the total size of the server installation
    pub async fn calculate_size(&self) -> Result<u64> {
        let server_path = self.server_path();
        if !Self::path_exists(&server_path).await {
            return Ok(0);
        }

        let mut total_size = 0u64;
        for entry in std::fs::read_dir(&server_path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                total_size += metadata.len();
            } else if metadata.is_dir() {
                total_size += self.calculate_dir_size(entry.path())?;
            }
        }

        Ok(total_size)
    }

    /// Recursively calculate directory size
    fn calculate_dir_size(&self, path: PathBuf) -> Result<u64> {
        let mut total_size = 0u64;
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                total_size += metadata.len();
            } else if metadata.is_dir() {
                total_size += self.calculate_dir_size(entry.path())?;
            }
        }

        Ok(total_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_paths() {
        let server = Server::new(
            GameId::Endfield,
            ServerId::CnOfficial,
            PathBuf::from("C:\\Games\\Endfield"),
        );

        assert_eq!(
            server.server_path(),
            PathBuf::from("C:\\Games\\Endfield\\servers\\cn_official")
        );
        assert_eq!(
            server.active_path(),
            PathBuf::from("C:\\Games\\Endfield\\active")
        );
        assert_eq!(
            server.game_exe_path(),
            PathBuf::from("C:\\Games\\Endfield\\servers\\cn_official\\Endfield.exe")
        );
    }
}

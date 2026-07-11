use std::path::Path;

use crate::api::ApiClient;
use crate::config::InstallProfile;
use crate::error::{Error, Result};

fn files_base_url(file_path: &str) -> String {
    let normalized = file_path.trim_end_matches('/');
    normalized
        .strip_suffix("/game_files")
        .unwrap_or(normalized)
        .to_string()
}

fn game_files_url(file_path: &str) -> String {
    let normalized = file_path.trim_end_matches('/');
    if normalized.ends_with("/game_files") {
        normalized.to_string()
    } else {
        format!("{normalized}/game_files")
    }
}

pub async fn sync_launcher_metadata(
    api_client: &ApiClient,
    install_path: &Path,
    profile: &InstallProfile,
    version: Option<&str>,
) -> Result<()> {
    let version_info = api_client.get_latest_game(&profile.target, version).await?;
    let pkg = version_info
        .pkg
        .as_ref()
        .ok_or_else(|| Error::ApiClient("No package information available".to_string()))?;

    let files_base_url = files_base_url(&pkg.file_path);
    let config_ini_url = format!("{files_base_url}/config.ini");
    let config_ini_path = install_path.join("config.ini");
    api_client
        .download_file(&config_ini_url, &config_ini_path, false)
        .await
        .map_err(|e| Error::Download(format!("Failed to sync launcher config.ini metadata: {e}")))?;

    let game_files_url = game_files_url(&pkg.file_path);
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

    let package_files_url = format!("{files_base_url}/package_files");
    let package_files_path = install_path.join("package_files");
    let _ = api_client
        .download_file(&package_files_url, &package_files_path, false)
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{files_base_url, game_files_url};

    #[test]
    fn files_base_url_handles_game_files_suffix() {
        assert_eq!(
            files_base_url("https://cdn.example.com/path/files/game_files"),
            "https://cdn.example.com/path/files"
        );
        assert_eq!(
            files_base_url("https://cdn.example.com/path/files"),
            "https://cdn.example.com/path/files"
        );
    }

    #[test]
    fn game_files_url_handles_both_shapes() {
        assert_eq!(
            game_files_url("https://cdn.example.com/path/files"),
            "https://cdn.example.com/path/files/game_files"
        );
        assert_eq!(
            game_files_url("https://cdn.example.com/path/files/game_files"),
            "https://cdn.example.com/path/files/game_files"
        );
    }
}

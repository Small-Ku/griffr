use std::path::Path;

use crate::api::ApiClient;
use crate::config::InstallTarget;
use crate::error::{Error, Result};
use crate::runtime::{launcher_metadata_url, CONFIG_INI_NAME, GAME_FILES_NAME, PACKAGE_FILES_NAME};

pub async fn sync_launcher_metadata(
    api_client: &ApiClient,
    install_path: &Path,
    install_target: &InstallTarget,
    version: Option<&str>,
) -> Result<()> {
    let version_info = api_client
        .get_latest_game(&install_target.api, version)
        .await?;
    let pkg = version_info
        .pkg
        .as_ref()
        .ok_or_else(|| Error::ApiClient("No package information available".to_string()))?;

    let config_ini_url = launcher_metadata_url(&pkg.file_path, CONFIG_INI_NAME)?;
    let config_ini_path = install_path.join(CONFIG_INI_NAME);
    api_client
        .download_file(&config_ini_url, &config_ini_path, false)
        .await
        .map_err(|e| {
            Error::Download(format!("Failed to sync launcher config.ini metadata: {e}"))
        })?;

    let game_files_url = launcher_metadata_url(&pkg.file_path, GAME_FILES_NAME)?;
    let game_files_path = install_path.join(GAME_FILES_NAME);
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

    let package_files_url = launcher_metadata_url(&pkg.file_path, PACKAGE_FILES_NAME)?;
    let package_files_path = install_path.join(PACKAGE_FILES_NAME);
    let _ = api_client
        .download_file(&package_files_url, &package_files_path, false)
        .await;

    Ok(())
}

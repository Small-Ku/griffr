use std::io::ErrorKind;
use std::path::Path;

use crate::api::ApiClient;
use crate::config::InstallTarget;
use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::{
    commit_unchecked_artifact, commit_verified_artifact, make_temp_write_path,
};
use crate::runtime::{
    launcher_metadata_url, ArtifactExpectation, ArtifactSource, CONFIG_INI_NAME, GAME_FILES_NAME,
    PACKAGE_FILES_NAME,
};

async fn download_metadata_file(
    api_client: &ApiClient,
    url: &str,
    destination: &Path,
    expected_md5: Option<&str>,
) -> Result<()> {
    let temp = make_temp_write_path(destination)?;
    match compio::fs::remove_file(&temp).await {
        Ok(()) => {}
        Err(source) if source.kind() == ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::IoAt {
                action: "remove file or directory",
                path: temp,
                source,
            });
        }
    }

    let download = match expected_md5 {
        Some(expected_md5) => {
            api_client
                .download_file_with_verify(url, &temp, expected_md5)
                .await
        }
        None => api_client
            .download_file(url, &temp, false)
            .await
            .map(|_| ()),
    };
    if let Err(error) = download {
        let _ = compio::fs::remove_file(&temp).await;
        return Err(error);
    }

    let commit_result = match expected_md5 {
        Some(expected_md5) => {
            let logical_path = destination
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| destination.to_string_lossy().into_owned());
            let expectation = ArtifactExpectation::new(logical_path, expected_md5, None);
            commit_verified_artifact(&temp, destination, &expectation, ArtifactSource::Download)
                .map(|_| ())
        }
        None => commit_unchecked_artifact(&temp, destination),
    };
    if let Err(error) = commit_result {
        let _ = compio::fs::remove_file(&temp).await;
        return Err(error);
    }
    Ok(())
}

pub async fn sync_launcher_metadata(
    api_client: &ApiClient,
    install_path: &Path,
    install_target: &InstallTarget,
    version: Option<&str>,
) -> Result<()> {
    let version_info = api_client
        .get_latest_game(&install_target.api, version)
        .await?;
    let pkg = version_info.pkg.as_ref().ok_or_else(|| Error::Message {
        context: "API client wrapper error: ",
        detail: "No package information available".to_string(),
    })?;

    let game_files_url = launcher_metadata_url(&pkg.file_path, GAME_FILES_NAME)?;
    let game_files_path = install_path.join(GAME_FILES_NAME);
    download_metadata_file(
        api_client,
        &game_files_url,
        &game_files_path,
        pkg.game_files_md5.as_deref(),
    )
    .await
    .map_err(|e| Error::Message {
        context: "Download error: ",
        detail: format!("Failed to sync launcher game_files metadata: {e}"),
    })?;

    let package_files_url = launcher_metadata_url(&pkg.file_path, PACKAGE_FILES_NAME)?;
    let package_files_path = install_path.join(PACKAGE_FILES_NAME);
    let _ = download_metadata_file(api_client, &package_files_url, &package_files_path, None).await;

    // `config.ini` is the installed-version source of truth. Write it only
    // after the required game_files manifest succeeds so an interrupted sync
    // cannot advertise the target version before its manifest is ready.
    let config_ini_url = launcher_metadata_url(&pkg.file_path, CONFIG_INI_NAME)?;
    let config_ini_path = install_path.join(CONFIG_INI_NAME);
    download_metadata_file(api_client, &config_ini_url, &config_ini_path, None)
        .await
        .map_err(|e| Error::Message {
            context: "Download error: ",
            detail: format!("Failed to sync launcher config.ini metadata: {e}"),
        })?;

    Ok(())
}

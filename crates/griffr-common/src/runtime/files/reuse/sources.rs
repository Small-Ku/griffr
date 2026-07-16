use std::path::{Path, PathBuf};

use crate::config::GameId;
use crate::error::{Error, Result};
use crate::runtime::{detect_local_install, LocalInstall};

use super::SourceInstallInput;

/// Inspect explicit reuse paths, reject incompatible games, and omit the
/// destination itself. The returned installations retain their launcher
/// metadata so frontends can derive command-specific reuse inputs.
pub async fn inspect_reuse_installations(
    game_id: &GameId,
    destination: &Path,
    source_paths: &[PathBuf],
) -> Result<Vec<LocalInstall>> {
    let mut sources = Vec::new();

    for source_path in source_paths {
        let source = detect_local_install(source_path).await.map_err(|error| {
            Error::Config(format!(
                "Failed to inspect reuse source {}: {error}",
                source_path.display()
            ))
        })?;
        let source_game_id = source.require_known_game()?;
        if &source_game_id != game_id {
            return Err(Error::Config(format!(
                "Reuse source {} is {}, expected {}",
                source.install_path.display(),
                source_game_id,
                game_id
            )));
        }
        if source.install_path != destination {
            sources.push(source);
        }
    }

    Ok(sources)
}

/// Resolve inspected installations into the metadata required by
/// the manifest-driven game-file ensure operation.
pub async fn resolve_file_reuse_sources(
    game_id: &GameId,
    destination: &Path,
    source_paths: &[PathBuf],
) -> Result<Vec<SourceInstallInput>> {
    let installations = inspect_reuse_installations(game_id, destination, source_paths).await?;
    let mut sources = Vec::with_capacity(installations.len());

    for source in installations {
        sources.push(SourceInstallInput {
            region_id: source.require_known_region()?,
            channel_id: source.require_known_channel()?,
            version: source.require_config_ini_version()?.to_string(),
            install_path: source.install_path,
        });
    }

    Ok(sources)
}

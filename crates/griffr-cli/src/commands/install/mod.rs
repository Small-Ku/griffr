mod run;

use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_runtime::{directory_has_entries, read_install_change, InstallChangeKind};

use crate::target::RemoteTarget;
use crate::{GlobalOptions, InstallTargetOverrideArgs};

#[allow(clippy::too_many_arguments)]
pub async fn install(
    target: RemoteTarget,
    overrides: InstallTargetOverrideArgs,
    install_path: PathBuf,
    force: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let pending_change = read_install_change(&install_path)?;
    let can_resume_install = pending_change
        .as_ref()
        .is_some_and(|state| state.kind == InstallChangeKind::Install);
    let install_path_exists = match compio::fs::metadata(&install_path).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to stat install path {}", install_path.display()))
        }
    };
    let install_path_had_entries =
        install_path_exists && directory_has_entries(install_path.clone()).await?;
    if install_path_had_entries && !force && !can_resume_install {
        anyhow::bail!(
            "Install path is not empty: {} (pass --force to reuse it)",
            install_path.display()
        );
    }

    match target {
        RemoteTarget::Hypergryph {
            game,
            region,
            channels,
        } => {
            run::install_hypergryph(
                game,
                region,
                channels,
                overrides,
                install_path,
                install_path_had_entries,
                reuse_paths,
                force_copy,
                opts,
            )
            .await
        }
        RemoteTarget::Yostar { game, region } => {
            crate::commands::yostar::install(
                game,
                region,
                install_path,
                install_path_had_entries,
                reuse_paths,
                force_copy,
                overrides,
                opts,
            )
            .await
        }
    }
}

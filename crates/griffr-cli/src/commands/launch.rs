use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::runtime::admin::ensure_admin;
use griffr_common::runtime::Launcher;

use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::detect_local_install;

pub async fn launch(path: PathBuf, force: bool, opts: GlobalOptions) -> Result<()> {
    ensure_admin().map_err(|e| anyhow::anyhow!("Failed to get administrator rights: {}", e))?;

    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let region_id = local.require_known_region()?;
    let channel_id = local.require_known_channel()?;
    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &Default::default(),
    )?;
    let launcher = Launcher::new(game_id.clone(), install_target, &local.install_path);
    let exe_path = launcher.game_exe_path()?;

    ui::print_phase(format!("Launching {} from {}", game_id, exe_path.display()));

    match compio::fs::metadata(&exe_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            anyhow::bail!("Program file not found: {}", exe_path.display());
        }
        Err(err) => {
            return Err(anyhow::Error::from(err)).with_context(|| {
                format!(
                    "Failed to read program file metadata for {}",
                    exe_path.display()
                )
            })
        }
    }

    if opts.is_dry_run() {
        opts.dry_run(format!("Would launch {}", exe_path.display()));
        return Ok(());
    }

    if launcher.is_game_running() {
        if !force {
            anyhow::bail!(
                "Game process already running at {}",
                local.install_path.display()
            );
        }
        ui::print_info("A game process is running; stop it because --force is set");
        launcher.stop_game().await?;
    }

    let _child = launcher.launch().await?;
    ui::print_success("Game process started");
    Ok(())
}

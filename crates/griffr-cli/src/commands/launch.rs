use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::game::admin::ensure_admin;
use griffr_common::game::Launcher;

use super::local::detect_local_install;
use crate::ui;
use crate::GlobalOptions;

pub async fn launch(path: PathBuf, force: bool, opts: GlobalOptions) -> Result<()> {
    ensure_admin()
        .map_err(|e| anyhow::anyhow!("Failed to obtain administrator privileges: {}", e))?;

    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let launcher = Launcher::new(game_id, &local.install_path);
    let exe_path = launcher.game_exe_path();

    ui::print_phase(format!("Launching {} from {}", game_id, exe_path.display()));

    match compio::fs::metadata(&exe_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            anyhow::bail!("Executable not found: {}", exe_path.display());
        }
        Err(err) => {
            return Err(err)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("Failed to stat executable {}", exe_path.display()))
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
        ui::print_info("Existing game process detected; terminating due to --force");
        launcher.kill_game().await?;
    }

    let _child = launcher.launch().await?;
    ui::print_success("Game process started");
    Ok(())
}

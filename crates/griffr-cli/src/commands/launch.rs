use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::runtime::admin::ensure_admin;
#[cfg(not(windows))]
use griffr_common::runtime::WineConfig;
use griffr_common::runtime::{ensure_install_ready, Launcher};

use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::detect_local_install;

pub async fn launch(
    path: PathBuf,
    force: bool,
    wine: Option<PathBuf>,
    wine_prefix: Option<PathBuf>,
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
    ensure_install_ready(&local.install_path)?;
    ensure_admin().map_err(|e| anyhow::anyhow!("Failed to get administrator rights: {}", e))?;

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
    #[cfg(windows)]
    let launcher = {
        if wine.is_some() || wine_prefix.is_some() {
            anyhow::bail!("--wine and --wine-prefix are only supported on non-Windows hosts");
        }
        launcher
    };
    #[cfg(not(windows))]
    let launcher = {
        let mut config = WineConfig::from_environment();
        if let Some(runner) = wine {
            config.runner = runner;
        }
        if let Some(prefix) = wine_prefix {
            config.prefix = Some(prefix);
        }
        launcher.with_wine_config(config)
    };
    let exe_path = launcher.game_exe_path()?;

    #[cfg(windows)]
    ui::print_phase(format!("Launching {} from {}", game_id, exe_path.display()));
    #[cfg(not(windows))]
    {
        let config = launcher
            .wine_config()
            .expect("non-Windows launcher must have Wine configuration");
        ui::print_phase(format!(
            "Launching {} from {} with {}",
            game_id,
            exe_path.display(),
            config.runner.display()
        ));
        if let Some(prefix) = config.effective_prefix() {
            ui::print_info(format!("Wine prefix: {}", prefix.display()));
        }
    }

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
        #[cfg(windows)]
        opts.dry_run(format!("Would launch {}", exe_path.display()));
        #[cfg(not(windows))]
        {
            let config = launcher
                .wine_config()
                .expect("non-Windows launcher must have Wine configuration");
            opts.dry_run(format!(
                "Would run {} {}",
                config.runner.display(),
                exe_path.display()
            ));
        }
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

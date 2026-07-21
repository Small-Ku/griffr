use crate::debug_cli::AccountCommands;
use crate::{commands, GlobalOptions};
use anyhow::Result;

pub(super) async fn dispatch_account(command: AccountCommands, opts: GlobalOptions) -> Result<()> {
    match command {
        AccountCommands::Capture {
            game,
            region_hint,
            bundle,
            sdk_dir,
            install_path,
            include_install_mmkv,
            force,
        } => {
            commands::account_capture(
                game,
                region_hint,
                bundle,
                sdk_dir,
                install_path,
                include_install_mmkv,
                force,
                opts,
            )
            .await?;
        }
        AccountCommands::Activate {
            game,
            region_hint,
            bundle,
            sdk_dir,
            install_path,
            include_install_mmkv,
            force,
        } => {
            commands::account_activate(
                game,
                region_hint,
                bundle,
                sdk_dir,
                install_path,
                include_install_mmkv,
                force,
                opts,
            )
            .await?;
        }
    }
    Ok(())
}

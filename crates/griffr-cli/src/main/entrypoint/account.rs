use crate::debug_cli::AccountCommands;
use crate::{commands, GlobalOptions};
use anyhow::Result;

pub(super) async fn dispatch_account(command: AccountCommands, opts: GlobalOptions) -> Result<()> {
    match command {
        AccountCommands::Capture {
            mutation,
            game,
            region_hint,
            bundle,
            sdk_dir,
            install_path,
            include_install_mmkv,
            force,
        } => {
            let opts = opts.with_dry_run(mutation.dry_run);
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
            mutation,
            game,
            region_hint,
            bundle,
            sdk_dir,
            install_path,
            include_install_mmkv,
            force,
        } => {
            let opts = opts.with_dry_run(mutation.dry_run);
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

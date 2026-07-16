use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::Result;
use griffr_common::runtime::{read_patch_storage_topology, remove_dir_all};

use crate::progress::ActivityProgress;
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::resolve_install_path;

pub async fn uninstall(
    path: PathBuf,
    keep_files: bool,
    yes: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let target = resolve_install_path(&path);

    ui::print_phase(format!("Uninstall target: {}", target.display()));

    if keep_files {
        ui::print_success("Skipped file deletion due to --keep-files");
        return Ok(());
    }

    if !yes && !opts.is_dry_run() {
        print!("delete {} ? [y/N]: ", target.display());
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            ui::print_info("Uninstall cancelled.");
            return Ok(());
        }
    }

    let external_vfs_root =
        read_patch_storage_topology(&target)?.map(|topology| topology.external_vfs_root);

    if opts.is_dry_run() {
        opts.dry_run(format!("Would delete {}", target.display()));
        if let Some(external) = external_vfs_root.as_ref() {
            opts.dry_run(format!(
                "Would also delete external VFS root {}",
                external.display()
            ));
        }
        return Ok(());
    }

    let exists = match compio::fs::metadata(&target).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => return Err(anyhow::Error::from(err)),
    };
    if exists {
        let progress = ActivityProgress::new(format!("Deleting {}", target.display()));
        if let Err(err) = remove_dir_all(target.clone()).await {
            progress.fail();
            return Err(err.into());
        }
        progress.finish();
        if let Some(external) = external_vfs_root {
            if external.exists() && !external.starts_with(&target) {
                let external_progress =
                    ActivityProgress::new(format!("Deleting external VFS {}", external.display()));
                if let Err(err) = remove_dir_all(external.clone()).await {
                    external_progress.fail();
                    return Err(err.into());
                }
                external_progress.finish();
            }
        }
        ui::print_success(format!("Deleted {}", target.display()));
    } else {
        ui::print_info("Target path does not exist; nothing to remove.");
    }

    Ok(())
}

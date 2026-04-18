use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::Result;

use crate::ui;
use crate::GlobalOptions;

pub async fn uninstall(
    path: PathBuf,
    keep_files: bool,
    yes: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let target = if path.is_dir() {
        path
    } else {
        path.parent().map(|p| p.to_path_buf()).unwrap_or(path)
    };

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

    if opts.is_dry_run() {
        opts.dry_run(format!("Would delete {}", target.display()));
        return Ok(());
    }

    let exists = match compio::fs::metadata(&target).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => return Err(anyhow::Error::from(err)),
    };
    if exists {
        std::fs::remove_dir_all(&target)?;
        ui::print_success(format!("Deleted {}", target.display()));
    } else {
        ui::print_info("Target path does not exist; nothing to remove.");
    }

    Ok(())
}

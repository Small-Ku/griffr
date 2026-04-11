use std::path::PathBuf;

use anyhow::Result;

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

    println!(
        "uninstall path={} keep_files={}",
        target.display(),
        keep_files
    );

    if keep_files {
        return Ok(());
    }

    if !yes && !opts.is_dry_run() {
        print!("delete {} ? [y/N]: ", target.display());
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            return Ok(());
        }
    }

    if opts.is_dry_run() {
        opts.dry_run(format!("Would delete {}", target.display()));
        return Ok(());
    }

    if target.exists() {
        tokio::fs::remove_dir_all(&target).await?;
    }

    Ok(())
}

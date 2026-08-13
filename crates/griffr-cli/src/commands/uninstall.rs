use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::runtime::{
    read_asset_storage_layout, read_yostar_metadata, remove_dir_all, resolve_install_path,
    ASSET_STORAGE_METADATA_NAME, CONFIG_INI_NAME, GRIFFR_DIR, YOSTAR_LAUNCHER_CONFIG_NAME,
    YOSTAR_MANIFEST_NAME,
};
use serde::{Deserialize, Serialize};

use crate::progress::ActivityProgress;
use crate::ui;
use crate::GlobalOptions;

const UNINSTALL_PLAN_NAME: &str = ".griffr-uninstall.json";

#[derive(Debug)]
struct UninstallTarget {
    root: PathBuf,
    storage: Option<griffr_common::runtime::AssetStorageLayout>,
    owned_external_root: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct UninstallPlan {
    schema_version: u32,
    install_root: PathBuf,
    external_asset_root: Option<PathBuf>,
}

pub async fn uninstall(path: PathBuf, detach: bool, yes: bool, opts: GlobalOptions) -> Result<()> {
    let target = inspect_uninstall_target(&path).await?;

    ui::print_phase(format!(
        "{} target: {}",
        if detach { "Detach" } else { "Uninstall" },
        target.root.display()
    ));

    if opts.is_dry_run() {
        if detach {
            opts.dry_run(format!(
                "Would remove Griffr private state from {} and keep game files",
                target.root.display()
            ));
            if target.owned_external_root.is_some() {
                opts.dry_run("Would release external asset ownership without deleting its files");
            }
        } else {
            opts.dry_run(format!("Would delete {}", target.root.display()));
            match (&target.storage, &target.owned_external_root) {
                (_, Some(external)) => opts.dry_run(format!(
                    "Would first delete owned external asset root {}",
                    external.display()
                )),
                (Some(layout), None) if !layout.external_asset_root.starts_with(&target.root) => {
                    ui::print_warning(format!(
                        "External asset root {} has no matching ownership sentinel and would be detached, not deleted",
                        layout.external_asset_root.display()
                    ));
                }
                _ => {}
            }
        }
        return Ok(());
    }

    if !yes {
        print!(
            "{} {} ? [y/N]: ",
            if detach { "detach" } else { "delete" },
            target.root.display()
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            ui::print_info("Operation cancelled.");
            return Ok(());
        }
    }

    if detach {
        detach_install(&target).await?;
        ui::print_success(format!(
            "Detached Griffr state from {}; game files were kept",
            target.root.display()
        ));
        return Ok(());
    }

    persist_uninstall_plan(&target).await?;
    run_yostar_uninstall_hook(&target.root).await;

    if let Some(external) = target.owned_external_root.as_ref() {
        if !external.starts_with(&target.root) {
            let progress = ActivityProgress::new(format!(
                "Deleting owned external assets {}",
                external.display()
            ));
            if let Err(error) = remove_dir_all(external.clone()).await {
                progress.fail();
                return Err(error.into());
            }
            progress.finish();
        }
    }

    let progress = ActivityProgress::new(format!("Deleting {}", target.root.display()));
    if let Err(error) = remove_dir_all(target.root.clone()).await {
        progress.fail();
        return Err(error.into());
    }
    progress.finish();
    ui::print_success(format!("Deleted {}", target.root.display()));
    Ok(())
}

async fn run_yostar_uninstall_hook(root: &Path) {
    if !root.join(YOSTAR_LAUNCHER_CONFIG_NAME).is_file()
        || !root.join(YOSTAR_MANIFEST_NAME).is_file()
    {
        return;
    }
    let metadata = match read_yostar_metadata(root).await {
        Ok(metadata) => metadata,
        Err(error) => {
            ui::print_warning(format!(
                "Could not validate YoStar launcher metadata before uninstall; native uninstall hook will be skipped: {error}"
            ));
            return;
        }
    };
    let Some(relative) = metadata.uninstall_script_relative_path() else {
        if !metadata.config.game_uninstall_script.trim().is_empty() {
            ui::print_warning(format!(
                "Skipping unsafe YoStar uninstall hook name {:?}",
                metadata.config.game_uninstall_script
            ));
        }
        return;
    };
    let script = root.join(relative);
    if !script.is_file() {
        return;
    }

    #[cfg(windows)]
    {
        let root = root.to_path_buf();
        let script = script.clone();
        let outcome = compio::runtime::spawn_blocking(move || {
            std::process::Command::new("cmd.exe")
                .arg("/c")
                .arg(&script)
                .current_dir(&root)
                .output()
        })
        .await;
        match outcome {
            Ok(Ok(output)) if output.status.success() => {}
            Ok(Ok(output)) => ui::print_warning(format!(
                "YoStar uninstall hook {} exited with {}",
                script.display(),
                output.status
            )),
            Ok(Err(error)) => ui::print_warning(format!(
                "Could not execute YoStar uninstall hook {}: {error}",
                script.display()
            )),
            Err(_) => ui::print_warning(format!(
                "YoStar uninstall hook task panicked for {}",
                script.display()
            )),
        }
    }

    #[cfg(not(windows))]
    ui::print_warning(format!(
        "YoStar native uninstall hook {} is a Windows batch file and was not executed on this host",
        script.display()
    ));
}

async fn inspect_uninstall_target(path: &Path) -> Result<UninstallTarget> {
    let candidate = resolve_install_path(path).await?;
    compio::runtime::spawn_blocking(move || inspect_uninstall_target_sync(&candidate))
        .await
        .map_err(|_| anyhow::anyhow!("uninstall path validation task panicked"))?
}

fn inspect_uninstall_target_sync(candidate: &Path) -> Result<UninstallTarget> {
    let root = validate_uninstall_root(candidate)?;
    let storage = read_asset_storage_layout(&root)?;
    let owned_external_root = match storage.as_ref() {
        Some(layout) if layout.external_asset_root.starts_with(&root) => None,
        Some(layout) if layout.owns_external_root(&root)? => {
            validate_destructive_root(&layout.external_asset_root, "external asset root")?;
            Some(layout.external_asset_root.clone())
        }
        _ => None,
    };

    Ok(UninstallTarget {
        root,
        storage,
        owned_external_root,
    })
}

fn validate_uninstall_root(candidate: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(candidate).with_context(|| {
        format!(
            "Install path {} does not exist or cannot be inspected",
            candidate.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "Refusing to uninstall through symlink or junction {}. Use the real install path.",
            candidate.display()
        );
    }
    if !metadata.is_dir() {
        anyhow::bail!("Install target {} is not a directory", candidate.display());
    }

    let root = std::fs::canonicalize(candidate)
        .with_context(|| format!("Failed to canonicalize {}", candidate.display()))?;
    validate_destructive_root(&root, "install root")?;

    let has_marker = root.join(CONFIG_INI_NAME).is_file()
        || (root.join(YOSTAR_LAUNCHER_CONFIG_NAME).is_file()
            && root.join(YOSTAR_MANIFEST_NAME).is_file())
        || root.join(GRIFFR_DIR).is_dir()
        || root.join(ASSET_STORAGE_METADATA_NAME).is_file();
    if !has_marker {
        anyhow::bail!(
            "Refusing to delete {} because it has no supported launcher metadata, {} directory, or {} ownership metadata",
            root.display(),
            GRIFFR_DIR,
            ASSET_STORAGE_METADATA_NAME
        );
    }
    Ok(root)
}

fn validate_destructive_root(path: &Path, role: &str) -> Result<()> {
    if path.parent().is_none() || path.components().count() <= 1 {
        anyhow::bail!(
            "Refusing to use filesystem root {} as {role}",
            path.display()
        );
    }
    if let Some(home) = dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok()) {
        if path == home {
            anyhow::bail!("Refusing to use the home directory as {role}");
        }
    }
    if let Ok(cwd) = std::env::current_dir().and_then(std::fs::canonicalize) {
        if cwd == path || cwd.starts_with(path) {
            anyhow::bail!(
                "Refusing to delete {role} {} while the current directory is inside it",
                path.display()
            );
        }
    }
    Ok(())
}

async fn persist_uninstall_plan(target: &UninstallTarget) -> Result<()> {
    let path = target.root.join(UNINSTALL_PLAN_NAME);
    let plan = UninstallPlan {
        schema_version: 1,
        install_root: target.root.clone(),
        external_asset_root: target.owned_external_root.clone(),
    };
    let payload = serde_json::to_vec_pretty(&plan)?;
    compio::fs::write(&path, payload)
        .await
        .0
        .with_context(|| format!("Failed to persist uninstall plan at {}", path.display()))?;
    Ok(())
}

async fn detach_install(target: &UninstallTarget) -> Result<()> {
    if let Some(layout) = target.storage.clone() {
        let root = target.root.clone();
        compio::runtime::spawn_blocking(move || layout.remove_owner_sentinel_if_owned(&root))
            .await
            .map_err(|_| anyhow::anyhow!("external ownership detach task panicked"))??;
    }

    let private_state = target.root.join(GRIFFR_DIR);
    match compio::fs::metadata(&private_state).await {
        Ok(_) => remove_dir_all(private_state).await?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    for name in [ASSET_STORAGE_METADATA_NAME, UNINSTALL_PLAN_NAME] {
        let path = target.root.join(name);
        match compio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to remove {}", path.display()))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_child_is_never_replaced_by_parent() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(CONFIG_INI_NAME), b"marker").unwrap();
        let missing = temp.path().join("missing-install");

        let error = validate_uninstall_root(&missing).unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert!(temp.path().exists());
    }

    #[test]
    fn arbitrary_directory_without_install_marker_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("ordinary");
        std::fs::create_dir_all(&target).unwrap();

        let error = validate_uninstall_root(&target).unwrap_err();
        assert!(error.to_string().contains("no supported launcher metadata"));
    }

    #[test]
    fn yostar_install_directory_is_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("game");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(YOSTAR_LAUNCHER_CONFIG_NAME), b"{}").unwrap();
        std::fs::write(target.join(YOSTAR_MANIFEST_NAME), b"{}").unwrap();

        assert_eq!(
            validate_uninstall_root(&target).unwrap(),
            std::fs::canonicalize(target).unwrap()
        );
    }

    #[test]
    fn recognizable_install_directory_is_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("game");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(CONFIG_INI_NAME), b"marker").unwrap();

        assert_eq!(
            validate_uninstall_root(&target).unwrap(),
            std::fs::canonicalize(target).unwrap()
        );
    }
}

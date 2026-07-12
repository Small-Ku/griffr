use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use griffr_common::config::{
    game_catalog_entry, local_low_vendor, ChannelId, GameId, GRYPHLINE_LOCAL_LOW_VENDOR,
    HYPERGRYPH_LOCAL_LOW_VENDOR,
};
use griffr_common::runtime::{copy_dir_recursive, remove_dir_all};

use crate::ui;
use crate::GlobalOptions;

const BUNDLE_SDK_DIR: &str = "sdk_data";
const BUNDLE_MMKV_DIR: &str = "mmkv";

pub(super) async fn create_dir_all(path: &Path) -> Result<()> {
    compio::fs::create_dir_all(path)
        .await
        .with_context(|| format!("Failed to create {}", path.display()))?;
    Ok(())
}

pub async fn capture(
    game_id: GameId,
    channel_hint: Option<ChannelId>,
    bundle_path: PathBuf,
    sdk_dir: Option<PathBuf>,
    install_path: Option<PathBuf>,
    include_install_mmkv: bool,
    force: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let source_sdk_dir = resolve_source_sdk_dir(game_id, channel_hint, sdk_dir.as_deref())?;
    let bundle_sdk_dir = bundle_path.join(BUNDLE_SDK_DIR);

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would capture account state from {} to {}",
            source_sdk_dir.display(),
            bundle_sdk_dir.display()
        ));
        if include_install_mmkv {
            let install_path = install_path
                .as_ref()
                .context("Missing --install-path while --include-install-mmkv is set")?;
            opts.dry_run(format!(
                "Would capture optional install mmkv from {} to {}",
                install_path.join("mmkv").display(),
                bundle_path.join(BUNDLE_MMKV_DIR).display()
            ));
        }
        return Ok(());
    }

    ensure_destination_dir(&bundle_path, force)
        .await
        .with_context(|| {
            format!(
                "Refusing to write bundle destination {}",
                bundle_path.display()
            )
        })?;
    create_dir_all(&bundle_path).await?;

    let sdk_stats = copy_dir_recursive(source_sdk_dir.clone(), bundle_sdk_dir.clone())
        .await
        .with_context(|| {
            format!(
                "Failed to capture sdk_data from {}",
                source_sdk_dir.display()
            )
        })?;

    ui::print_info(format!(
        "Captured sdk_data: files={} size={} source={} target={}",
        sdk_stats.files,
        ui::format_bytes(sdk_stats.bytes),
        source_sdk_dir.display(),
        bundle_sdk_dir.display()
    ));

    if include_install_mmkv {
        let install_path = install_path
            .as_ref()
            .context("Missing --install-path while --include-install-mmkv is set")?;
        let source_mmkv = install_path.join("mmkv");
        if !source_mmkv.is_dir() {
            anyhow::bail!(
                "Install mmkv path does not exist: {} (omit --include-install-mmkv or provide a compatible install)",
                source_mmkv.display()
            );
        }
        let bundle_mmkv = bundle_path.join(BUNDLE_MMKV_DIR);
        let mmkv_stats = copy_dir_recursive(source_mmkv.clone(), bundle_mmkv.clone())
            .await
            .with_context(|| {
                format!(
                    "Failed to capture install mmkv from {}",
                    source_mmkv.display()
                )
            })?;
        ui::print_info(format!(
            "Captured install mmkv: files={} size={} source={} target={}",
            mmkv_stats.files,
            ui::format_bytes(mmkv_stats.bytes),
            source_mmkv.display(),
            bundle_mmkv.display()
        ));
    }

    ui::print_success("Account capture complete");
    Ok(())
}

pub async fn activate(
    game_id: GameId,
    channel_hint: Option<ChannelId>,
    bundle_path: PathBuf,
    sdk_dir: Option<PathBuf>,
    install_path: Option<PathBuf>,
    include_install_mmkv: bool,
    force: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let bundle_sdk_dir = bundle_path.join(BUNDLE_SDK_DIR);
    if !bundle_sdk_dir.is_dir() {
        anyhow::bail!(
            "Bundle is missing sdk_data payload: {}",
            bundle_sdk_dir.display()
        );
    }

    let target_sdk_dir = resolve_target_sdk_dir(game_id, channel_hint, sdk_dir.as_deref())?;
    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would activate account state from {} to {}",
            bundle_sdk_dir.display(),
            target_sdk_dir.display()
        ));
        if include_install_mmkv {
            let install_path = install_path
                .as_ref()
                .context("Missing --install-path while --include-install-mmkv is set")?;
            opts.dry_run(format!(
                "Would restore optional install mmkv from {} to {}",
                bundle_path.join(BUNDLE_MMKV_DIR).display(),
                install_path.join("mmkv").display()
            ));
        }
        return Ok(());
    }

    ensure_destination_dir(&target_sdk_dir, force)
        .await
        .with_context(|| {
            format!(
                "Refusing to overwrite target sdk_data directory {} without --force",
                target_sdk_dir.display()
            )
        })?;

    if let Some(parent) = target_sdk_dir.parent() {
        create_dir_all(parent).await?;
    }

    let sdk_stats = copy_dir_recursive(bundle_sdk_dir.clone(), target_sdk_dir.clone())
        .await
        .with_context(|| {
            format!(
                "Failed to restore sdk_data from bundle {}",
                bundle_sdk_dir.display()
            )
        })?;
    ui::print_info(format!(
        "Activated sdk_data: files={} size={} source={} target={}",
        sdk_stats.files,
        ui::format_bytes(sdk_stats.bytes),
        bundle_sdk_dir.display(),
        target_sdk_dir.display()
    ));

    if include_install_mmkv {
        let install_path = install_path
            .as_ref()
            .context("Missing --install-path while --include-install-mmkv is set")?;
        let bundle_mmkv = bundle_path.join(BUNDLE_MMKV_DIR);
        if !bundle_mmkv.is_dir() {
            anyhow::bail!(
                "Bundle is missing optional mmkv payload: {}",
                bundle_mmkv.display()
            );
        }
        let target_mmkv = install_path.join("mmkv");
        ensure_destination_dir(&target_mmkv, force)
            .await
            .with_context(|| {
                format!(
                    "Refusing to overwrite install mmkv {} without --force",
                    target_mmkv.display()
                )
            })?;
        let mmkv_stats = copy_dir_recursive(bundle_mmkv.clone(), target_mmkv.clone())
            .await
            .with_context(|| {
                format!(
                    "Failed to restore install mmkv into {}",
                    target_mmkv.display()
                )
            })?;
        ui::print_info(format!(
            "Activated install mmkv: files={} size={} source={} target={}",
            mmkv_stats.files,
            ui::format_bytes(mmkv_stats.bytes),
            bundle_mmkv.display(),
            target_mmkv.display()
        ));
    }

    ui::print_success("Account activate complete");
    Ok(())
}

pub(super) fn resolve_source_sdk_dir(
    game_id: GameId,
    channel_hint: Option<ChannelId>,
    sdk_dir: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(explicit) = sdk_dir {
        validate_explicit_sdk_dir(explicit)?;
        return Ok(explicit.to_path_buf());
    }
    select_latest_sdk_dir_from_roots(&default_game_local_low_roots(game_id, channel_hint)?)
}

pub(super) fn resolve_target_sdk_dir(
    game_id: GameId,
    channel_hint: Option<ChannelId>,
    sdk_dir: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(explicit) = sdk_dir {
        if let Some(name) = explicit.file_name().and_then(OsStr::to_str) {
            if !name.starts_with("sdk_data_") {
                anyhow::bail!(
                    "Explicit --sdk-dir must end with sdk_data_* (got {})",
                    explicit.display()
                );
            }
        }
        return Ok(explicit.to_path_buf());
    }
    select_latest_sdk_dir_from_roots(&default_game_local_low_roots(game_id, channel_hint)?)
}

pub(super) fn validate_explicit_sdk_dir(path: &Path) -> Result<()> {
    if !path.is_dir() {
        anyhow::bail!("SDK dir not found: {}", path.display());
    }
    if let Some(name) = path.file_name().and_then(OsStr::to_str) {
        if !name.starts_with("sdk_data_") {
            anyhow::bail!("Expected sdk_data_* directory, got {}", path.display());
        }
    }
    Ok(())
}

pub(super) fn default_game_local_low_roots(
    game_id: GameId,
    channel_hint: Option<ChannelId>,
) -> Result<Vec<PathBuf>> {
    let user_profile = std::env::var("USERPROFILE")
        .context("USERPROFILE is not set; this command requires Windows user profile context")?;
    let game_dir = game_catalog_entry(&game_id)
        .map(|game| game.local_low_dir)
        .context("Custom games must provide --sdk-dir; no LocalLow path is inferred")?;
    let base = PathBuf::from(user_profile).join("AppData").join("LocalLow");
    local_low_roots_for_hint(&base, game_dir, channel_hint)
}

pub(super) fn local_low_roots_for_hint(
    base: &Path,
    game_dir: &str,
    channel_hint: Option<ChannelId>,
) -> Result<Vec<PathBuf>> {
    if let Some(channel) = channel_hint {
        let vendor = local_low_vendor(&channel).with_context(|| {
            format!(
                "Custom channel {} must provide --sdk-dir; no LocalLow vendor is inferred",
                channel
            )
        })?;
        return Ok(vec![base.join(vendor).join(game_dir)]);
    }

    Ok(vec![
        base.join(HYPERGRYPH_LOCAL_LOW_VENDOR).join(game_dir),
        base.join(GRYPHLINE_LOCAL_LOW_VENDOR).join(game_dir),
    ])
}

#[cfg(test)]
pub(super) fn select_latest_sdk_dir(root: &Path) -> Result<PathBuf> {
    let mut candidates = Vec::<(PathBuf, SystemTime)>::new();
    let entries =
        std::fs::read_dir(root).with_context(|| format!("Failed to read {}", root.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to enumerate {}", root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        if !name.starts_with("sdk_data_") {
            continue;
        }
        let modified = std::fs::metadata(&path)
            .with_context(|| format!("Failed to stat {}", path.display()))?
            .modified()
            .with_context(|| format!("Failed to read modified timestamp for {}", path.display()))?;
        candidates.push((path, modified));
    }

    if candidates.is_empty() {
        anyhow::bail!(
            "No sdk_data_* directory found under {} (launch game once or pass --sdk-dir)",
            root.display()
        );
    }

    candidates.sort_by_key(|(_, modified)| *modified);
    let (path, _) = candidates
        .into_iter()
        .next_back()
        .context("Failed to choose sdk_data directory")?;
    Ok(path)
}

pub(super) fn select_latest_sdk_dir_from_roots(roots: &[PathBuf]) -> Result<PathBuf> {
    let mut candidates = Vec::<(PathBuf, SystemTime)>::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let entries = std::fs::read_dir(root)
            .with_context(|| format!("Failed to read {}", root.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("Failed to enumerate {}", root.display()))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
            if !name.starts_with("sdk_data_") {
                continue;
            }
            let modified = std::fs::metadata(&path)
                .with_context(|| format!("Failed to stat {}", path.display()))?
                .modified()
                .with_context(|| {
                    format!("Failed to read modified timestamp for {}", path.display())
                })?;
            candidates.push((path, modified));
        }
    }

    if candidates.is_empty() {
        anyhow::bail!(
            "No sdk_data_* directory found under any default roots: {} (launch game once or pass --sdk-dir)",
            roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    candidates.sort_by_key(|(_, modified)| *modified);
    let (path, _) = candidates
        .into_iter()
        .next_back()
        .context("Failed to choose sdk_data directory")?;
    Ok(path)
}

pub(super) async fn ensure_destination_dir(path: &Path, force: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if !force {
        anyhow::bail!("Destination exists: {}", path.display());
    }
    if path.is_file() {
        compio::fs::remove_file(path)
            .await
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    } else {
        remove_dir_all(path.to_path_buf()).await?;
    }
    Ok(())
}

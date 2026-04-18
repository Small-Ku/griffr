use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use griffr_common::config::{GameId, ServerId};

use crate::ui;
use crate::GlobalOptions;

const BUNDLE_SDK_DIR: &str = "sdk_data";
const BUNDLE_MMKV_DIR: &str = "mmkv";

pub async fn capture(
    game_id: GameId,
    server_hint: Option<ServerId>,
    bundle_path: PathBuf,
    sdk_dir: Option<PathBuf>,
    install_path: Option<PathBuf>,
    include_install_mmkv: bool,
    force: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let source_sdk_dir = resolve_source_sdk_dir(game_id, server_hint, sdk_dir.as_deref())?;
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

    ensure_destination_dir(&bundle_path, force).with_context(|| {
        format!(
            "Refusing to write bundle destination {}",
            bundle_path.display()
        )
    })?;
    std::fs::create_dir_all(&bundle_path)
        .with_context(|| format!("Failed to create {}", bundle_path.display()))?;

    let sdk_stats = copy_dir_recursive(&source_sdk_dir, &bundle_sdk_dir).with_context(|| {
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
        let mmkv_stats = copy_dir_recursive(&source_mmkv, &bundle_mmkv).with_context(|| {
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
    server_hint: Option<ServerId>,
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

    let target_sdk_dir = resolve_target_sdk_dir(game_id, server_hint, sdk_dir.as_deref())?;
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

    ensure_destination_dir(&target_sdk_dir, force).with_context(|| {
        format!(
            "Refusing to overwrite target sdk_data directory {} without --force",
            target_sdk_dir.display()
        )
    })?;

    if let Some(parent) = target_sdk_dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let sdk_stats = copy_dir_recursive(&bundle_sdk_dir, &target_sdk_dir).with_context(|| {
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
        ensure_destination_dir(&target_mmkv, force).with_context(|| {
            format!(
                "Refusing to overwrite install mmkv {} without --force",
                target_mmkv.display()
            )
        })?;
        let mmkv_stats = copy_dir_recursive(&bundle_mmkv, &target_mmkv).with_context(|| {
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

fn resolve_source_sdk_dir(
    game_id: GameId,
    server_hint: Option<ServerId>,
    sdk_dir: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(explicit) = sdk_dir {
        validate_explicit_sdk_dir(explicit)?;
        return Ok(explicit.to_path_buf());
    }
    select_latest_sdk_dir_from_roots(&default_game_local_low_roots(game_id, server_hint)?)
}

fn resolve_target_sdk_dir(
    game_id: GameId,
    server_hint: Option<ServerId>,
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
    select_latest_sdk_dir_from_roots(&default_game_local_low_roots(game_id, server_hint)?)
}

fn validate_explicit_sdk_dir(path: &Path) -> Result<()> {
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

fn default_game_local_low_roots(
    game_id: GameId,
    server_hint: Option<ServerId>,
) -> Result<Vec<PathBuf>> {
    let user_profile = std::env::var("USERPROFILE")
        .context("USERPROFILE is not set; this command requires Windows user profile context")?;
    let game_dir = match game_id {
        GameId::Arknights => "Arknights",
        GameId::Endfield => "Endfield",
    };
    let base = PathBuf::from(user_profile).join("AppData").join("LocalLow");
    local_low_roots_for_hint(&base, game_id, game_dir, server_hint)
}

fn local_low_roots_for_hint(
    base: &Path,
    game_id: GameId,
    game_dir: &str,
    server_hint: Option<ServerId>,
) -> Result<Vec<PathBuf>> {
    if let Some(server) = server_hint {
        if !ServerId::available_for(game_id).contains(&server) {
            anyhow::bail!(
                "Server hint {} is not available for game {}",
                server,
                game_id
            );
        }
        let vendor = match server {
            ServerId::CnOfficial | ServerId::CnBilibili => "Hypergryph",
            ServerId::GlobalOfficial | ServerId::GlobalEpic => "Gryphline",
        };
        return Ok(vec![base.join(vendor).join(game_dir)]);
    }

    Ok(vec![
        base.join("Hypergryph").join(game_dir),
        base.join("Gryphline").join(game_dir),
    ])
}

#[cfg(test)]
fn select_latest_sdk_dir(root: &Path) -> Result<PathBuf> {
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

fn select_latest_sdk_dir_from_roots(roots: &[PathBuf]) -> Result<PathBuf> {
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

fn ensure_destination_dir(path: &Path, force: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if !force {
        anyhow::bail!("Destination exists: {}", path.display());
    }
    if path.is_file() {
        std::fs::remove_file(path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    } else {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy)]
struct CopyStats {
    files: usize,
    bytes: u64,
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<CopyStats> {
    if !source.is_dir() {
        anyhow::bail!("Source directory not found: {}", source.display());
    }
    std::fs::create_dir_all(target)
        .with_context(|| format!("Failed to create {}", target.display()))?;
    let mut stats = CopyStats::default();
    copy_dir_recursive_inner(source, target, &mut stats)?;
    Ok(stats)
}

fn copy_dir_recursive_inner(source: &Path, target: &Path, stats: &mut CopyStats) -> Result<()> {
    let entries = std::fs::read_dir(source)
        .with_context(|| format!("Failed to read {}", source.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to enumerate {}", source.display()))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());

        if source_path.is_dir() {
            std::fs::create_dir_all(&target_path)
                .with_context(|| format!("Failed to create {}", target_path.display()))?;
            copy_dir_recursive_inner(&source_path, &target_path, stats)?;
        } else if source_path.is_file() {
            std::fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "Failed to copy {} -> {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
            let len = std::fs::metadata(&source_path)
                .with_context(|| format!("Failed to stat {}", source_path.display()))?
                .len();
            stats.files += 1;
            stats.bytes += len;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn select_latest_sdk_dir_prefers_newest_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let older = temp.path().join("sdk_data_old");
        let newer = temp.path().join("sdk_data_new");
        std::fs::create_dir_all(&older).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::create_dir_all(&newer).unwrap();

        let selected = select_latest_sdk_dir(temp.path()).unwrap();
        assert_eq!(selected, newer);
    }

    #[test]
    fn select_latest_sdk_dir_from_roots_prefers_newest_across_roots() {
        let temp = tempfile::tempdir().unwrap();
        let root_a = temp.path().join("Hypergryph").join("Endfield");
        let root_b = temp.path().join("Gryphline").join("Endfield");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();

        let older = root_a.join("sdk_data_older");
        let newer = root_b.join("sdk_data_newer");
        std::fs::create_dir_all(&older).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::create_dir_all(&newer).unwrap();

        let selected = select_latest_sdk_dir_from_roots(&[root_a.clone(), root_b.clone()]).unwrap();
        assert_eq!(selected, newer);
    }

    #[test]
    fn local_low_roots_for_hint_cn_prefers_hypergryph() {
        let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
        let roots = local_low_roots_for_hint(
            &base,
            GameId::Endfield,
            "Endfield",
            Some(ServerId::CnOfficial),
        )
        .unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], base.join("Hypergryph").join("Endfield"));
    }

    #[test]
    fn local_low_roots_for_hint_global_prefers_gryphline() {
        let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
        let roots = local_low_roots_for_hint(
            &base,
            GameId::Endfield,
            "Endfield",
            Some(ServerId::GlobalOfficial),
        )
        .unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], base.join("Gryphline").join("Endfield"));
    }

    #[test]
    fn local_low_roots_for_hint_rejects_invalid_server_for_game() {
        let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
        let err = local_low_roots_for_hint(
            &base,
            GameId::Arknights,
            "Arknights",
            Some(ServerId::GlobalOfficial),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not available for game"));
    }

    #[test]
    fn ensure_destination_dir_requires_force_when_existing() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("existing");
        std::fs::create_dir_all(&existing).unwrap();

        let err = ensure_destination_dir(&existing, false).unwrap_err();
        assert!(err.to_string().contains("Destination exists"));
    }

    #[test]
    fn copy_dir_recursive_copies_nested_content() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let nested = source.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("token_cache.bin");
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(&[1, 2, 3, 4]).unwrap();

        let target = temp.path().join("target");
        let stats = copy_dir_recursive(&source, &target).unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, 4);
        assert_eq!(
            std::fs::read(target.join("nested").join("token_cache.bin")).unwrap(),
            vec![1, 2, 3, 4]
        );
    }
}

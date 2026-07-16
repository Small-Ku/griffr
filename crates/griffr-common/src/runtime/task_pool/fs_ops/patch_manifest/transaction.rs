use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::runtime::patch_transaction::{
    read_patch_execution_plan, write_patch_execution_plan, write_patch_storage_topology,
    PatchExecutionPlan, PatchPreflightReport, PatchStorageTopology, PlannedPatchEntry,
    PlannedPatchSource, PATCH_DEFERRED_DIR, PATCH_TRANSACTION_DIR,
};
use crate::runtime::task_pool::verify::{build_issue, file_md5};
use crate::runtime::{DELETE_FILES_MANIFEST_NAME, PATCH_MANIFEST_NAME, PATCH_STAGE_DIR};

use super::super::extract::move_path_replace_cross_volume;
use super::materialize::{apply_hdiff_patch, verify_materialized_file};

fn remove_path_if_exists(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|source| Error::RemoveFailed {
                path: path.to_path_buf(),
                source,
            })
        }
        Ok(_) => std::fs::remove_file(path).map_err(|source| Error::RemoveFailed {
            path: path.to_path_buf(),
            source,
        }),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::StatFailed {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn move_file_cross_volume(source: &Path, target: &Path) -> Result<()> {
    move_path_replace_cross_volume(source, target)
}

fn move_directory_contents(source: &Path, target: &Path) -> Result<()> {
    if !source.exists() {
        std::fs::create_dir_all(target).map_err(|source_error| Error::CreateDirFailed {
            path: target.to_path_buf(),
            source: source_error,
        })?;
        return Ok(());
    }
    std::fs::create_dir_all(target).map_err(|source_error| Error::CreateDirFailed {
        path: target.to_path_buf(),
        source: source_error,
    })?;
    for entry in std::fs::read_dir(source).map_err(|source_error| Error::ReadDirFailed {
        path: source.to_path_buf(),
        source: source_error,
    })? {
        let entry = entry.map_err(|source_error| Error::ReadDirFailed {
            path: source.to_path_buf(),
            source: source_error,
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|source_error| Error::StatFailed {
                path: source_path.clone(),
                source: source_error,
            })?;
        if file_type.is_dir() {
            move_directory_contents(&source_path, &target_path)?;
            let _ = std::fs::remove_dir(&source_path);
        } else {
            if target_path.exists() {
                let source_metadata =
                    std::fs::metadata(&source_path).map_err(|source_error| Error::StatFailed {
                        path: source_path.clone(),
                        source: source_error,
                    })?;
                let target_metadata =
                    std::fs::metadata(&target_path).map_err(|source_error| Error::StatFailed {
                        path: target_path.clone(),
                        source: source_error,
                    })?;
                if !target_metadata.is_file()
                    || source_metadata.len() != target_metadata.len()
                    || file_md5(&source_path)? != file_md5(&target_path)?
                {
                    return Err(Error::Vfs(format!(
                        "External VFS relocation conflict at {}",
                        target_path.display()
                    )));
                }
                std::fs::remove_file(&source_path).map_err(|source_error| Error::RemoveFailed {
                    path: source_path.clone(),
                    source: source_error,
                })?;
                continue;
            }
            move_file_cross_volume(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn create_directory_link(link: &Path, target: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).map_err(|source| Error::Other(format!(
            "Failed to create external VFS directory link {} -> {}: {}. Enable Windows Developer Mode or run with permission to create symbolic links",
            link.display(),
            target.display(),
            source
        )))
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map_err(|source| {
            Error::Other(format!(
                "Failed to create external VFS directory link {} -> {}: {}",
                link.display(),
                target.display(),
                source
            ))
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (link, target);
        Err(Error::Other(
            "External VFS roots are unsupported on this platform".to_string(),
        ))
    }
}

fn prepare_external_vfs_root(plan: &PatchExecutionPlan) -> Result<()> {
    let logical = plan.install_root.join(&plan.vfs_base_path);
    if logical == plan.vfs_destination {
        return Ok(());
    }
    if let Ok(existing_target) = std::fs::read_link(&logical) {
        let resolved_target = if existing_target.is_absolute() {
            existing_target.clone()
        } else {
            logical
                .parent()
                .unwrap_or(Path::new("."))
                .join(&existing_target)
        };
        if resolved_target == plan.vfs_destination {
            std::fs::create_dir_all(&plan.vfs_destination).map_err(|source| {
                Error::CreateDirFailed {
                    path: plan.vfs_destination.clone(),
                    source,
                }
            })?;
            return write_patch_storage_topology(
                &plan.install_root,
                &PatchStorageTopology {
                    schema_version: PatchStorageTopology::SCHEMA_VERSION,
                    vfs_link: plan.vfs_base_path.clone(),
                    external_vfs_root: plan.vfs_destination.clone(),
                },
            );
        }
        return Err(Error::Vfs(format!(
            "VFS path {} already links to {}, not requested external root {}",
            logical.display(),
            existing_target.display(),
            plan.vfs_destination.display()
        )));
    }
    if let Some(parent) = logical.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    move_directory_contents(&logical, &plan.vfs_destination)?;
    if logical.exists() {
        std::fs::remove_dir_all(&logical).map_err(|source| Error::RemoveFailed {
            path: logical.clone(),
            source,
        })?;
    }
    create_directory_link(&logical, &plan.vfs_destination)?;
    write_patch_storage_topology(
        &plan.install_root,
        &PatchStorageTopology {
            schema_version: PatchStorageTopology::SCHEMA_VERSION,
            vfs_link: plan.vfs_base_path.clone(),
            external_vfs_root: plan.vfs_destination.clone(),
        },
    )
}

fn collect_staged_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| Error::ReadDirFailed {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::ReadDirFailed {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| Error::StatFailed {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn is_patch_control_path(relative: &Path) -> bool {
    relative == Path::new(PATCH_MANIFEST_NAME)
        || relative == Path::new(DELETE_FILES_MANIFEST_NAME)
        || relative.starts_with(PATCH_STAGE_DIR)
}

fn commit_top_level_files(
    plan: &PatchExecutionPlan,
    mut callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let files = collect_staged_files(&plan.stage_root)?;
    let deferred = plan.deferred_paths.iter().cloned().collect::<BTreeSet<_>>();
    let commit_files = files
        .into_iter()
        .filter_map(|source| {
            let relative = source.strip_prefix(&plan.stage_root).ok()?.to_path_buf();
            (!is_patch_control_path(&relative)).then_some((source, relative))
        })
        .collect::<Vec<_>>();
    let total = commit_files.len();
    if total > 0 {
        if let Some(callback) = callback.as_deref_mut() {
            callback(Path::new("."), 0, total);
        }
    }
    for (index, (source, relative)) in commit_files.into_iter().enumerate() {
        let target = if deferred.contains(&relative) {
            plan.install_root
                .join(PATCH_TRANSACTION_DIR)
                .join(PATCH_DEFERRED_DIR)
                .join(&relative)
        } else {
            plan.install_root.join(&relative)
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source_error| Error::CreateDirFailed {
                path: parent.to_path_buf(),
                source: source_error,
            })?;
        }
        move_path_replace_cross_volume(&source, &target)?;
        if let Some(callback) = callback.as_deref_mut() {
            callback(&relative, index + 1, total);
        }
    }
    Ok(())
}

fn materialize_entry(plan: &PatchExecutionPlan, entry: &PlannedPatchEntry) -> Result<()> {
    if build_issue(
        &entry.destination,
        &entry.name,
        &entry.expected_md5,
        Some(entry.expected_size),
    )
    .is_none()
    {
        return Ok(());
    }
    match &entry.source {
        PlannedPatchSource::AlreadyMaterialized => Err(Error::Vfs(format!(
            "Patch output {} no longer matches its completed state",
            entry.destination.display()
        ))),
        PlannedPatchSource::Local { payload } => {
            let source = plan.stage_root.join(payload);
            if !source.is_file() {
                return Err(Error::Vfs(format!(
                    "Missing local patch payload {} for {}",
                    source.display(),
                    entry.name
                )));
            }
            if let Some(parent) = entry.destination.parent() {
                std::fs::create_dir_all(parent).map_err(|source_error| Error::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source: source_error,
                })?;
            }
            move_path_replace_cross_volume(&source, &entry.destination)?;
            verify_materialized_file(
                &entry.destination,
                &entry.name,
                &entry.expected_md5,
                entry.expected_size,
            )
        }
        PlannedPatchSource::Hdiff {
            base,
            payload,
            base_md5,
            base_size,
        } => {
            let base_logical = relative_install_path(plan, base)
                .unwrap_or_else(|| base.to_path_buf())
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(issue) = build_issue(base, &base_logical, base_md5, Some(*base_size)) {
                return Err(Error::Vfs(format!(
                    "Patch base {} failed verification before materializing {}: {:?}",
                    base.display(),
                    entry.name,
                    issue.kind
                )));
            }
            let payload_path = plan.stage_root.join(payload);
            if !payload_path.is_file() {
                return Err(Error::Vfs(format!(
                    "Missing HDiff payload {} for {}",
                    payload_path.display(),
                    entry.name
                )));
            }
            apply_hdiff_patch(
                base,
                &payload_path,
                &entry.destination,
                &entry.name,
                &entry.expected_md5,
                entry.expected_size,
                plan.work_dir.as_deref(),
            )?;
            remove_path_if_exists(&payload_path)
        }
    }
}

fn logical_vfs_root(plan: &PatchExecutionPlan) -> PathBuf {
    plan.install_root.join(&plan.vfs_base_path)
}

fn physical_delete_path(plan: &PatchExecutionPlan, relative: &Path) -> PathBuf {
    if plan.vfs_destination != logical_vfs_root(plan) {
        if let Ok(vfs_relative) = relative.strip_prefix(&plan.vfs_base_path) {
            return plan.vfs_destination.join(vfs_relative);
        }
    }
    plan.install_root.join(relative)
}

fn relative_install_path(plan: &PatchExecutionPlan, path: &Path) -> Option<PathBuf> {
    if let Ok(vfs_relative) = path.strip_prefix(&plan.vfs_destination) {
        return Some(plan.vfs_base_path.join(vfs_relative));
    }
    path.strip_prefix(&plan.install_root)
        .ok()
        .map(Path::to_path_buf)
}

fn final_output_paths(plan: &PatchExecutionPlan) -> BTreeSet<PathBuf> {
    plan.entries
        .iter()
        .map(|entry| plan.vfs_base_path.join(&entry.name))
        .collect()
}

fn ordered_entries(plan: &PatchExecutionPlan) -> Result<Vec<&PlannedPatchEntry>> {
    let mut destination_writers = BTreeMap::new();
    for (index, entry) in plan.entries.iter().enumerate() {
        let logical_destination = plan
            .install_root
            .join(&plan.vfs_base_path)
            .join(&entry.name);
        let aliases = [entry.destination.clone(), logical_destination]
            .into_iter()
            .collect::<BTreeSet<_>>();
        for destination in aliases {
            if destination_writers
                .insert(destination.clone(), index)
                .is_some()
            {
                return Err(Error::Vfs(format!(
                    "Patch plan has multiple writers for {}",
                    destination.display()
                )));
            }
        }
    }

    let mut outgoing = vec![BTreeSet::<usize>::new(); plan.entries.len()];
    let mut indegree = vec![0usize; plan.entries.len()];
    for (consumer_index, entry) in plan.entries.iter().enumerate() {
        let PlannedPatchSource::Hdiff { base, .. } = &entry.source else {
            continue;
        };
        let Some(&writer_index) = destination_writers.get(base) else {
            continue;
        };
        if writer_index == consumer_index {
            continue;
        }
        if outgoing[consumer_index].insert(writer_index) {
            indegree[writer_index] = indegree[writer_index].saturating_add(1);
        }
    }

    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(plan.entries.len());
    while let Some(index) = ready.pop_first() {
        order.push(index);
        for dependent in outgoing[index].iter().copied() {
            indegree[dependent] = indegree[dependent].saturating_sub(1);
            if indegree[dependent] == 0 {
                ready.insert(dependent);
            }
        }
    }
    if order.len() != plan.entries.len() {
        let blocked = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| {
                (*degree > 0).then_some(plan.entries[index].name.as_str())
            })
            .collect::<Vec<_>>();
        return Err(Error::Vfs(format!(
            "Patch dependency graph contains a destructive overwrite cycle: {}",
            blocked.join(", ")
        )));
    }
    Ok(order
        .into_iter()
        .map(|index| &plan.entries[index])
        .collect())
}

fn delete_unreferenced_paths_before_patch(plan: &PatchExecutionPlan) -> Result<()> {
    let bases = plan
        .entries
        .iter()
        .filter_map(|entry| match &entry.source {
            PlannedPatchSource::Hdiff { base, .. } => Some(base.clone()),
            PlannedPatchSource::AlreadyMaterialized | PlannedPatchSource::Local { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let outputs = plan
        .entries
        .iter()
        .map(|entry| entry.destination.clone())
        .collect::<BTreeSet<_>>();
    for relative in &plan.delete_paths {
        let path = physical_delete_path(plan, relative);
        if !bases.contains(&path) && !outputs.contains(&path) {
            remove_path_if_exists(&path)?;
        }
    }
    Ok(())
}

fn release_base_if_unused(
    plan: &PatchExecutionPlan,
    base: &Path,
    remaining: &mut BTreeMap<PathBuf, usize>,
    delete_set: &BTreeSet<PathBuf>,
    outputs: &BTreeSet<PathBuf>,
) -> Result<()> {
    let Some(count) = remaining.get_mut(base) else {
        return Ok(());
    };
    *count = count.saturating_sub(1);
    if *count != 0 {
        return Ok(());
    }
    let Some(relative) = relative_install_path(plan, base) else {
        return Ok(());
    };
    if delete_set.contains(&relative) && !outputs.contains(&relative) {
        remove_path_if_exists(base)?;
    }
    Ok(())
}

fn apply_remaining_deletes(
    plan: &PatchExecutionPlan,
    mut callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let total = plan.delete_paths.len();
    if total > 0 {
        if let Some(callback) = callback.as_deref_mut() {
            callback(Path::new("."), 0, total);
        }
    }
    let outputs = final_output_paths(plan);
    for (index, relative) in plan.delete_paths.iter().enumerate() {
        if !outputs.contains(relative) {
            remove_path_if_exists(&physical_delete_path(plan, relative))?;
        }
        if let Some(callback) = callback.as_deref_mut() {
            callback(relative, index + 1, total);
        }
    }
    Ok(())
}

fn commit_deferred_files(plan: &PatchExecutionPlan) -> Result<()> {
    let deferred_root = plan
        .install_root
        .join(PATCH_TRANSACTION_DIR)
        .join(PATCH_DEFERRED_DIR);
    for relative in &plan.deferred_paths {
        let source = deferred_root.join(relative);
        if !source.is_file() {
            continue;
        }
        let target = plan.install_root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source_error| Error::CreateDirFailed {
                path: parent.to_path_buf(),
                source: source_error,
            })?;
        }
        move_path_replace_cross_volume(&source, &target)?;
    }
    Ok(())
}

fn cleanup_staging(plan: &PatchExecutionPlan) -> Result<()> {
    if plan.stage_root.exists() {
        std::fs::remove_dir_all(&plan.stage_root).map_err(|source| Error::RemoveFailed {
            path: plan.stage_root.clone(),
            source,
        })?;
    }
    Ok(())
}

fn cleanup_transaction(plan: &PatchExecutionPlan) -> Result<()> {
    let transaction_root = plan.install_root.join(PATCH_TRANSACTION_DIR);
    if transaction_root.exists() {
        std::fs::remove_dir_all(&transaction_root).map_err(|source| Error::RemoveFailed {
            path: transaction_root,
            source,
        })?;
    }
    Ok(())
}

pub(crate) fn execute_patch_transaction(
    plan: &PatchExecutionPlan,
    _report: Option<&PatchPreflightReport>,
    commit_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    mut patch_callback: Option<&mut dyn FnMut(&str, usize, usize)>,
    delete_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    plan.validate()?;
    write_patch_execution_plan(plan)?;
    prepare_external_vfs_root(plan)?;
    commit_top_level_files(plan, commit_callback)?;
    delete_unreferenced_paths_before_patch(plan)?;

    let delete_set = plan.delete_paths.iter().cloned().collect::<BTreeSet<_>>();
    let outputs = final_output_paths(plan);
    let mut remaining = BTreeMap::<PathBuf, usize>::new();
    for entry in &plan.entries {
        if let PlannedPatchSource::Hdiff { base, .. } = &entry.source {
            *remaining.entry(base.clone()).or_default() += 1;
        }
    }
    let ordered_entries = ordered_entries(plan)?;
    let total = ordered_entries.len();
    if total > 0 {
        if let Some(callback) = patch_callback.as_deref_mut() {
            callback("", 0, total);
        }
    }
    for (index, entry) in ordered_entries.into_iter().enumerate() {
        materialize_entry(plan, entry).map_err(|error| {
            Error::Other(format!(
                "Failed to materialize patch entry {}: {}",
                entry.name, error
            ))
        })?;
        if let PlannedPatchSource::Hdiff { base, .. } = &entry.source {
            release_base_if_unused(plan, base, &mut remaining, &delete_set, &outputs)?;
        }
        if let Some(callback) = patch_callback.as_deref_mut() {
            callback(&entry.name, index + 1, total);
        }
    }
    apply_remaining_deletes(plan, delete_callback)?;
    commit_deferred_files(plan)?;
    cleanup_staging(plan)?;
    cleanup_transaction(plan)
}

pub(crate) fn resume_patch_transaction(
    install_root: &Path,
    commit_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
    patch_callback: Option<&mut dyn FnMut(&str, usize, usize)>,
    delete_callback: Option<&mut dyn FnMut(&Path, usize, usize)>,
) -> Result<()> {
    let plan = read_patch_execution_plan(install_root)?;
    execute_patch_transaction(
        &plan,
        None,
        commit_callback,
        patch_callback,
        delete_callback,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn plan(
        install_root: &Path,
        stage_root: &Path,
        entries: Vec<PlannedPatchEntry>,
        delete_paths: Vec<PathBuf>,
        deferred_paths: Vec<PathBuf>,
    ) -> PatchExecutionPlan {
        PatchExecutionPlan {
            schema_version: PatchExecutionPlan::SCHEMA_VERSION,
            install_root: install_root.to_path_buf(),
            stage_root: stage_root.to_path_buf(),
            vfs_base_path: PathBuf::from("Game_Data/StreamingAssets/VFS"),
            vfs_destination: install_root.join("Game_Data/StreamingAssets/VFS"),
            work_dir: None,
            entries,
            delete_paths,
            deferred_paths,
        }
    }

    #[test]
    fn transaction_defers_version_marker_and_preserves_final_output() {
        let temp = tempdir().unwrap();
        let install_root = temp.path().join("install");
        let stage_root = temp.path().join("stage");
        let output = install_root.join("Game_Data/StreamingAssets/VFS/final.bin");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&stage_root).unwrap();
        std::fs::write(&output, b"final").unwrap();
        std::fs::write(stage_root.join("config.ini"), b"version=2").unwrap();
        std::fs::write(stage_root.join("top-level.bin"), b"replacement").unwrap();
        std::fs::write(install_root.join("config.ini"), b"version=1").unwrap();

        let plan = plan(
            &install_root,
            &stage_root,
            vec![PlannedPatchEntry {
                name: "final.bin".to_string(),
                destination: output.clone(),
                expected_md5: file_md5(&output).unwrap(),
                expected_size: 5,
                source: PlannedPatchSource::AlreadyMaterialized,
            }],
            Vec::new(),
            vec![PathBuf::from("config.ini")],
        );

        execute_patch_transaction(&plan, None, None, None, None).unwrap();

        assert_eq!(std::fs::read(&output).unwrap(), b"final");
        assert_eq!(
            std::fs::read(install_root.join("top-level.bin")).unwrap(),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(install_root.join("config.ini")).unwrap(),
            b"version=2"
        );
        assert!(!stage_root.exists());
        assert!(!install_root.join(PATCH_TRANSACTION_DIR).exists());
    }

    #[test]
    fn plan_rejects_delete_manifest_conflict_with_output() {
        let temp = tempdir().unwrap();
        let install_root = temp.path().join("install");
        let stage_root = temp.path().join("stage");
        let output = install_root.join("Game_Data/StreamingAssets/VFS/final.bin");
        let plan = plan(
            &install_root,
            &stage_root,
            vec![PlannedPatchEntry {
                name: "final.bin".to_string(),
                destination: output,
                expected_md5: "a".to_string(),
                expected_size: 1,
                source: PlannedPatchSource::AlreadyMaterialized,
            }],
            vec![PathBuf::from("Game_Data/StreamingAssets/VFS/final.bin")],
            Vec::new(),
        );

        assert!(plan.validate().is_err());
    }

    #[test]
    fn base_is_released_only_after_last_consumer() {
        let temp = tempdir().unwrap();
        let install_root = temp.path().join("install");
        let stage_root = temp.path().join("stage");
        let base = install_root.join("Game_Data/StreamingAssets/VFS/old.bin");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, b"old").unwrap();
        let plan = plan(
            &install_root,
            &stage_root,
            Vec::new(),
            vec![PathBuf::from("Game_Data/StreamingAssets/VFS/old.bin")],
            Vec::new(),
        );
        let mut remaining = BTreeMap::from([(base.clone(), 2usize)]);
        let deletes = plan.delete_paths.iter().cloned().collect();
        let outputs = BTreeSet::new();

        release_base_if_unused(&plan, &base, &mut remaining, &deletes, &outputs).unwrap();
        assert!(base.exists());
        release_base_if_unused(&plan, &base, &mut remaining, &deletes, &outputs).unwrap();
        assert!(!base.exists());
    }

    #[test]
    fn materialization_revalidates_persisted_base_metadata() {
        let temp = tempdir().unwrap();
        let install_root = temp.path().join("install");
        let stage_root = temp.path().join("stage");
        let base = install_root.join("Game_Data/StreamingAssets/VFS/base.bin");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, b"changed").unwrap();

        let entry = PlannedPatchEntry {
            name: "output.bin".to_string(),
            destination: install_root.join("Game_Data/StreamingAssets/VFS/output.bin"),
            expected_md5: "00000000000000000000000000000000".to_string(),
            expected_size: 1,
            source: PlannedPatchSource::Hdiff {
                base,
                payload: PathBuf::from("vfs_files/vfs_patch/output.patch"),
                base_md5: "11111111111111111111111111111111".to_string(),
                base_size: 7,
            },
        };
        let plan = plan(
            &install_root,
            &stage_root,
            vec![entry.clone()],
            Vec::new(),
            Vec::new(),
        );

        let error = materialize_entry(&plan, &entry).unwrap_err();
        assert!(error
            .to_string()
            .contains("failed verification before materializing"));
    }

    #[test]
    fn dependency_order_uses_logical_path_for_external_vfs() {
        let temp = tempdir().unwrap();
        let install_root = temp.path().join("install");
        let stage_root = temp.path().join("stage");
        let external = temp.path().join("external");
        let logical_base = install_root.join("Game_Data/StreamingAssets/VFS/intermediate.bin");
        let consumer = PlannedPatchEntry {
            name: "final.bin".to_string(),
            destination: external.join("final.bin"),
            expected_md5: "a".to_string(),
            expected_size: 1,
            source: PlannedPatchSource::Hdiff {
                base: logical_base,
                payload: PathBuf::from("vfs_files/vfs_patch/final.patch"),
                base_md5: "c".to_string(),
                base_size: 1,
            },
        };
        let writer = PlannedPatchEntry {
            name: "intermediate.bin".to_string(),
            destination: external.join("intermediate.bin"),
            expected_md5: "b".to_string(),
            expected_size: 1,
            source: PlannedPatchSource::Local {
                payload: PathBuf::from("vfs_files/files/intermediate.bin"),
            },
        };
        let mut plan = plan(
            &install_root,
            &stage_root,
            vec![writer, consumer],
            Vec::new(),
            Vec::new(),
        );
        plan.vfs_destination = external;

        let order = ordered_entries(&plan).unwrap();
        assert_eq!(order[0].name, "final.bin");
        assert_eq!(order[1].name, "intermediate.bin");
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::download::extractor::safe_relative_archive_path;
use crate::runtime::task_pool::{ArchiveRangePriority, ArchiveSource, Task, TransferClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunClass {
    AsyncIo,
    Cpu,
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NetworkClass {
    General,
    Vfs,
    Archive,
    ArchiveBackground,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StorageReservation {
    pub(super) volume: String,
    pub(super) probe_path: PathBuf,
    pub(super) bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResourceRequest {
    pub(super) run: RunClass,
    pub(super) network: Option<NetworkClass>,
    pub(super) read_volumes: Vec<String>,
    pub(super) write_volumes: Vec<String>,
    pub(super) metadata_volumes: Vec<String>,
    pub(super) archive_save_volumes: Vec<String>,
    pub(super) archive_commit_volumes: Vec<(String, bool)>,
    pub(super) extract: bool,
    pub(super) mutation_paths: Vec<String>,
    pub(super) storage_reservations: Vec<StorageReservation>,
    pub(super) estimated_bytes: u64,
    pub(super) reuse_probe: bool,
    pub(super) reuse_commit: bool,
}

impl Default for ResourceRequest {
    fn default() -> Self {
        Self {
            run: RunClass::Blocking,
            network: None,
            read_volumes: Vec::new(),
            write_volumes: Vec::new(),
            metadata_volumes: Vec::new(),
            archive_save_volumes: Vec::new(),
            archive_commit_volumes: Vec::new(),
            extract: false,
            mutation_paths: Vec::new(),
            storage_reservations: Vec::new(),
            estimated_bytes: 0,
            reuse_probe: false,
            reuse_commit: false,
        }
    }
}

pub(super) fn task_resources(task: &Task) -> ResourceRequest {
    let mut request = ResourceRequest {
        run: run_class(task),
        ..ResourceRequest::default()
    };
    match task {
        Task::FetchArchiveRange {
            request: range,
            priority,
            ..
        } => {
            request.network = Some(match priority {
                ArchiveRangePriority::ExtractionCritical => NetworkClass::Archive,
                ArchiveRangePriority::RetentionBackground => NetworkClass::ArchiveBackground,
            });
            route_archive_range_write(&mut request, range);
        }
        Task::FetchArchiveRepairFile { repair } => {
            request.network = Some(NetworkClass::Archive);
            if let Ok(ranges) = repair
                .work
                .layout
                .missing_range_requests([repair.source_range.clone()])
            {
                for range in ranges {
                    route_archive_range_write(&mut request, &range);
                }
            }
        }
        Task::ExtractArchiveRepairFile { repair } => {
            request.read_volumes.extend(
                repair
                    .work
                    .paths_for_indices(&repair.volume_indices)
                    .iter()
                    .map(|path| volume_key(path)),
            );
            let staging_volume = volume_key(&repair.staging_dir);
            let destination_volume = volume_key(&repair.dest);
            request.write_volumes.push(staging_volume.clone());
            request.write_volumes.push(destination_volume.clone());
            reserve_storage(&mut request, &repair.staging_dir, repair.expected_size);
            if staging_volume != destination_volume {
                reserve_storage(&mut request, &repair.dest, repair.expected_size);
            }
            request.mutation_paths.push(path_key(&repair.dest));
            request.extract = true;
        }
        Task::Verify { path, .. } => request.read_volumes.push(volume_key(path)),
        Task::Download {
            dest,
            expected_size,
            transfer_class,
            resume,
            ..
        } => {
            let volume = volume_key(dest);
            if resume.is_some() {
                request.network = Some(match transfer_class {
                    TransferClass::General => NetworkClass::General,
                    TransferClass::Vfs => NetworkClass::Vfs,
                });
                request.write_volumes.push(volume);
                if let (Some(expected_size), Some(resume)) = (expected_size, resume.as_ref()) {
                    reserve_storage(&mut request, dest, resume.remaining_bytes(*expected_size));
                }
            } else {
                request.read_volumes.push(volume.clone());
                request.metadata_volumes.push(volume);
            }
            request.mutation_paths.push(path_key(dest));
            if resume.is_some() {
                if let Ok(part_path) =
                    crate::runtime::task_pool::fs_ops::make_partial_download_path(dest)
                {
                    request.mutation_paths.push(path_key(&part_path));
                }
            }
        }
        Task::VerifyReuseVolume { candidates, .. } => {
            if let Some(path) = candidates.first() {
                request.read_volumes.push(volume_key(path));
            }
            request.reuse_probe = true;
        }
        Task::ReuseFile {
            source,
            copy_only,
            dest,
            expected_size,
            ..
        } => {
            if *copy_only {
                request.read_volumes.push(volume_key(source));
                request.write_volumes.push(volume_key(dest));
                reserve_storage(&mut request, dest, *expected_size);
                request.mutation_paths.push(path_key(dest));
            } else {
                let volume = volume_key(dest);
                request.read_volumes.push(volume.clone());
                request.metadata_volumes.push(volume);
                request.mutation_paths.push(path_key(dest));
                request.reuse_commit = true;
            }
        }
        Task::OpenArchive { source, .. } => match source {
            ArchiveSource::Remote(parts) => {
                for part in parts {
                    let volume = volume_key(&part.dest);
                    request.read_volumes.push(volume.clone());
                    request.metadata_volumes.push(volume);
                    request.mutation_paths.push(path_key(&part.dest));
                }
            }
            ArchiveSource::Local(volumes) => {
                for volume_path in volumes {
                    let volume = volume_key(volume_path);
                    request.read_volumes.push(volume.clone());
                    request.metadata_volumes.push(volume);
                }
            }
        },
        Task::DiscoverArchiveDirectory {
            work,
            required_range,
        } => {
            let mut indices = work
                .layout
                .volume_indices_for_range(work.layout.tail_probe_range());
            if let Some(range) = required_range {
                indices.extend(work.layout.volume_indices_for_range(range.clone()));
                indices.sort_unstable();
                indices.dedup();
            }
            request.read_volumes.extend(
                work.paths_for_indices(&indices)
                    .iter()
                    .map(|path| volume_key(path)),
            );
        }
        Task::InspectArchiveIndex { work, directory } => {
            let mut indices = work
                .layout
                .volume_indices_for_range(directory.central_directory.clone());
            indices.extend(
                work.layout
                    .volume_indices_for_range(directory.end_records.clone()),
            );
            indices.sort_unstable();
            indices.dedup();
            request.read_volumes.extend(
                work.paths_for_indices(&indices)
                    .iter()
                    .map(|path| volume_key(path)),
            );
        }
        Task::ReadArchiveControls {
            work,
            archive_index,
        } => {
            let indices = crate::download::extractor::MultiVolumeExtractor::control_volume_indices(
                archive_index,
            );
            request.read_volumes.extend(
                work.paths_for_indices(&indices)
                    .iter()
                    .map(|path| volume_key(path)),
            );
            request.read_volumes.push(volume_key(&work.dest));
            let staging_parent = work
                .patch_options
                .work_dir
                .as_deref()
                .or_else(|| work.dest.parent())
                .unwrap_or(work.dest.as_path());
            request.metadata_volumes.push(volume_key(staging_parent));
        }
        Task::ProbePatchArtifact {
            patch_check,
            probe_index,
        } => {
            if let Some(path) = patch_check.probe_path(*probe_index) {
                request.read_volumes.push(volume_key(path));
            }
        }
        Task::MeasurePatchRelocation { patch_check } => {
            if let Some(path) = patch_check.relocation_root() {
                request.read_volumes.push(volume_key(path));
            }
        }
        Task::SavePatchPlan { work, .. } => {
            request.read_volumes.push(volume_key(&work.dest));
            let staging_parent = work
                .patch_options
                .work_dir
                .as_deref()
                .or_else(|| work.dest.parent())
                .unwrap_or(work.dest.as_path());
            request.metadata_volumes.push(volume_key(staging_parent));
        }
        Task::ExtractArchiveShard { shard } => {
            request.read_volumes.extend(
                shard
                    .work
                    .paths_for_indices(&shard.volume_indices)
                    .iter()
                    .map(|path| volume_key(path)),
            );
            let staging_volume = volume_key(&shard.staging_dir);
            request.write_volumes.push(staging_volume.clone());
            if shard.direct_commit.is_some() {
                let peak_staging_bytes = shard
                    .entries
                    .iter()
                    .map(|index| shard.archive_index.entry_sizes[*index])
                    .max()
                    .unwrap_or(0);
                let mut destination_totals = BTreeMap::<String, (PathBuf, u64)>::new();
                for entry_index in &shard.entries {
                    let name = shard
                        .archive_index
                        .archive
                        .name_for_index(*entry_index)
                        .expect("validated archive index is missing an entry name");
                    if name.ends_with('/') {
                        continue;
                    }
                    let relative = safe_relative_archive_path(name)
                        .expect("validated archive index contains an unsafe entry path");
                    let destination = shard.work.dest.join(relative);
                    let destination_volume = volume_key(&destination);
                    let cross_volume = staging_volume != destination_volume;
                    accumulate_storage(
                        &mut destination_totals,
                        &destination,
                        shard.archive_index.entry_sizes[*entry_index],
                    );
                    if cross_volume {
                        request.read_volumes.push(staging_volume.clone());
                        request.read_volumes.push(destination_volume.clone());
                        request.write_volumes.push(destination_volume.clone());
                    } else {
                        request.metadata_volumes.push(destination_volume.clone());
                    }
                    request
                        .archive_commit_volumes
                        .push((destination_volume, cross_volume));
                    request.mutation_paths.push(path_key(&destination));
                }
                if !destination_totals.contains_key(&staging_volume) {
                    reserve_storage(&mut request, &shard.staging_dir, peak_staging_bytes);
                }
                for (_, (path, bytes)) in destination_totals {
                    reserve_storage(&mut request, &path, bytes);
                }
            } else {
                let staged_bytes = shard.entries.iter().fold(0u64, |total, index| {
                    total.saturating_add(shard.archive_index.entry_sizes[*index])
                });
                reserve_storage(&mut request, &shard.staging_dir, staged_bytes);
            }
            request.extract = true;
        }
        Task::RetainArchiveVolume { work, volume_index } => {
            let full_volume_is_ready = work
                .layout
                .volume_range(*volume_index)
                .is_some_and(|range| work.layout.range_is_available(&range));
            if let Some(path) = work.layout.path(*volume_index) {
                let volume = volume_key(path);
                if full_volume_is_ready {
                    request.read_volumes.push(volume);
                } else {
                    request.metadata_volumes.push(volume);
                }
            }
            if full_volume_is_ready {
                if let Some(part) = work.parts.get(*volume_index) {
                    let volume = volume_key(&part.dest);
                    request.write_volumes.push(volume.clone());
                    request.archive_save_volumes.push(volume);
                    reserve_storage(&mut request, &part.dest, part.expected_size);
                    request.mutation_paths.push(path_key(&part.dest));
                }
            }
        }
        Task::FinishArchive { work } => {
            if let Some(prepared) = work
                .prepared
                .lock()
                .unwrap()
                .as_ref()
                .filter(|prepared| prepared.patch_plan.is_none())
            {
                let staging_volume = volume_key(&prepared.staging_dir);
                let mut destination_totals = BTreeMap::<String, (PathBuf, u64)>::new();
                request.metadata_volumes.push(staging_volume.clone());
                request.mutation_paths.push(path_key(&prepared.staging_dir));
                for relative in &prepared.deferred_commit_paths {
                    let destination = work.dest.join(relative);
                    let destination_volume = volume_key(&destination);
                    let cross_volume = staging_volume != destination_volume;
                    if cross_volume {
                        request.read_volumes.push(staging_volume.clone());
                        request.read_volumes.push(destination_volume.clone());
                        request.write_volumes.push(destination_volume.clone());
                        let source = prepared.staging_dir.join(relative);
                        accumulate_storage(
                            &mut destination_totals,
                            &destination,
                            existing_file_len(&source),
                        );
                    } else {
                        request.metadata_volumes.push(destination_volume.clone());
                    }
                    request
                        .archive_commit_volumes
                        .push((destination_volume, cross_volume));
                    request.mutation_paths.push(path_key(&destination));
                }
                for (_, (path, bytes)) in destination_totals {
                    reserve_storage(&mut request, &path, bytes);
                }
            }
        }
        Task::PreparePatchApply { patch } => {
            route_prepare_patch_apply(&mut request, patch.plan());
        }
        Task::ApplyPatchEntry { patch, entry_index } => {
            if let Some(entry) = patch.entry(*entry_index) {
                match &entry.source {
                    crate::runtime::PlannedPatchSource::AlreadyPresent => {
                        request.read_volumes.push(volume_key(&entry.destination));
                    }
                    crate::runtime::PlannedPatchSource::Local { payload } => {
                        let source = patch.plan().stage_root.join(payload);
                        let source_volume = volume_key(&source);
                        let destination_volume = volume_key(&entry.destination);
                        request.read_volumes.push(source_volume.clone());
                        request.write_volumes.push(destination_volume.clone());
                        request.mutation_paths.push(path_key(&source));
                        if source_volume != destination_volume {
                            reserve_storage(&mut request, &entry.destination, entry.expected_size);
                        }
                    }
                    crate::runtime::PlannedPatchSource::Hdiff { base, payload, .. } => {
                        let payload_path = patch.plan().stage_root.join(payload);
                        request.read_volumes.push(volume_key(base));
                        request.read_volumes.push(volume_key(&payload_path));
                        request.mutation_paths.push(path_key(&payload_path));
                        request.write_volumes.push(volume_key(&entry.destination));
                        if let Some(work_dir) = patch.plan().work_dir.as_deref() {
                            request.write_volumes.push(volume_key(work_dir));
                            reserve_storage(&mut request, work_dir, entry.expected_size);
                            reserve_storage(&mut request, &entry.destination, entry.expected_size);
                        } else {
                            reserve_storage(&mut request, &entry.destination, entry.expected_size);
                        }
                    }
                }
                request.mutation_paths.push(path_key(&entry.destination));
            }
        }
        Task::ReleasePatchBase { base, .. } => {
            request.metadata_volumes.push(volume_key(base));
            request.mutation_paths.push(path_key(base));
        }
        Task::ApplyPatchDeletes { patch } => {
            for relative in &patch.plan().delete_paths {
                let path = physical_patch_path(patch.plan(), relative);
                request.metadata_volumes.push(volume_key(&path));
                request.mutation_paths.push(path_key(&path));
            }
        }
        Task::CommitPatchDeferred { patch } => {
            let plan = patch.plan();
            let mut destination_totals = BTreeMap::<String, (PathBuf, u64)>::new();
            let deferred_root = crate::runtime::griffr_patch_path(&plan.install_root)
                .join(crate::runtime::PATCH_DEFERRED_DIR);
            for relative in &plan.deferred_paths {
                let source = deferred_root.join(relative);
                let source_volume = volume_key(&source);
                request.read_volumes.push(source_volume.clone());
                request.mutation_paths.push(path_key(&source));
                let target = plan.install_root.join(relative);
                let destination_volume = volume_key(&target);
                request.write_volumes.push(destination_volume.clone());
                if source_volume != destination_volume {
                    accumulate_storage(
                        &mut destination_totals,
                        &target,
                        existing_file_len(&source),
                    );
                }
                request.mutation_paths.push(path_key(&target));
            }
            for (_, (path, bytes)) in destination_totals {
                reserve_storage(&mut request, &path, bytes);
            }
        }
        Task::CleanPatchApply { patch, archive: _ } => {
            let plan = patch.plan();
            let patch_root = crate::runtime::griffr_patch_path(&plan.install_root);
            request.metadata_volumes.push(volume_key(&plan.stage_root));
            request.metadata_volumes.push(volume_key(&patch_root));
            request.mutation_paths.push(path_key(&plan.stage_root));
            request.mutation_paths.push(path_key(&patch_root));
        }
        Task::CleanupArchive { work } => {
            request
                .metadata_volumes
                .extend(work.layout.paths().iter().map(|path| volume_key(path)));
        }
        Task::ApplyExtractedVfsPatchManifest { install_root } => {
            route_extracted_vfs_patch_manifest(&mut request, install_root);
        }
        Task::ApplyDeleteManifest { install_root } => {
            request.metadata_volumes.push(volume_key(install_root));
            request.mutation_paths.push(path_key(install_root));
        }
        Task::RepairFile { .. } => {}
    }
    request.estimated_bytes = task_estimated_bytes(task);
    normalize_volumes(&mut request);
    request
}

fn route_archive_range_write(
    request: &mut ResourceRequest,
    range: &crate::download::extractor::ArchiveRangeRequest,
) {
    let part_path = range.cache_path.with_extension("range.part");
    request.write_volumes.push(volume_key(&part_path));
    request.mutation_paths.push(path_key(&part_path));
    request.mutation_paths.push(path_key(&range.cache_path));
    reserve_remaining_file(
        request,
        &part_path,
        range.local_range.end - range.local_range.start,
    );
}

fn route_prepare_patch_apply(request: &mut ResourceRequest, plan: &crate::runtime::PatchPlan) {
    request.read_volumes.push(volume_key(&plan.stage_root));
    request.write_volumes.push(volume_key(&plan.install_root));
    request
        .write_volumes
        .push(volume_key(&plan.vfs_destination));
    if let Some(work_dir) = plan.work_dir.as_deref() {
        request.write_volumes.push(volume_key(work_dir));
    }
    request.mutation_paths.push(path_key(&plan.install_root));
    request.mutation_paths.push(path_key(&plan.stage_root));
    request.mutation_paths.push(path_key(&plan.vfs_destination));

    let logical_vfs_root = plan.install_root.join(&plan.vfs_base_path);
    if volume_key(&logical_vfs_root) != volume_key(&plan.vfs_destination)
        && std::fs::symlink_metadata(&logical_vfs_root)
            .ok()
            .is_some_and(|metadata| !metadata.file_type().is_symlink())
    {
        if let Ok(bytes) = crate::runtime::dir_size_sync(&logical_vfs_root) {
            reserve_storage(request, &plan.vfs_destination, bytes);
        }
    }

    let mut destination_totals = BTreeMap::<String, (PathBuf, u64)>::new();
    for (source, destination, bytes) in patch_prepare_commit_files(plan) {
        if volume_key(&source) != volume_key(&destination) {
            accumulate_storage(&mut destination_totals, &destination, bytes);
        }
    }
    for (_, (path, bytes)) in destination_totals {
        reserve_storage(request, &path, bytes);
    }
}

fn patch_prepare_commit_files(plan: &crate::runtime::PatchPlan) -> Vec<(PathBuf, PathBuf, u64)> {
    let deferred = plan.deferred_paths.iter().cloned().collect::<BTreeSet<_>>();
    let mut files = Vec::new();
    let mut pending = vec![plan.stage_root.clone()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let source = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&source) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(source);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let Ok(relative) = source.strip_prefix(&plan.stage_root) else {
                continue;
            };
            if is_patch_control_path(relative) {
                continue;
            }
            let destination = if deferred.contains(relative) {
                crate::runtime::griffr_patch_path(&plan.install_root)
                    .join(crate::runtime::PATCH_DEFERRED_DIR)
                    .join(relative)
            } else {
                plan.install_root.join(relative)
            };
            files.push((source, destination, metadata.len()));
        }
    }
    files
}

fn is_patch_control_path(relative: &Path) -> bool {
    relative == Path::new(crate::runtime::PATCH_MANIFEST_NAME)
        || relative == Path::new(crate::runtime::DELETE_FILES_MANIFEST_NAME)
        || relative.starts_with(crate::runtime::PATCH_STAGE_DIR)
}

fn route_extracted_vfs_patch_manifest(request: &mut ResourceRequest, install_root: &Path) {
    let manifest_path = install_root.join(crate::runtime::PATCH_MANIFEST_NAME);
    let stage_root = install_root.join(crate::runtime::PATCH_STAGE_DIR);
    request.read_volumes.push(volume_key(&manifest_path));
    let manifest = std::fs::read(&manifest_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::api::types::ResourcePatch>(&bytes).ok());
    let Some(manifest) = manifest else {
        let volume = volume_key(install_root);
        request.read_volumes.push(volume.clone());
        request.write_volumes.push(volume);
        request.mutation_paths.push(path_key(install_root));
        return;
    };
    let Ok(vfs_base_path) =
        crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "patch.json vfs_base_path",
            manifest.vfs_base_path.trim(),
        )
    else {
        request.mutation_paths.push(path_key(install_root));
        return;
    };
    request.read_volumes.push(volume_key(&stage_root));
    let destination_root = install_root.join(vfs_base_path);
    let mut destination_totals = BTreeMap::<String, (PathBuf, u64)>::new();
    for entry in manifest.files {
        let Ok(relative) = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "patch.json file name",
            &entry.name,
        ) else {
            continue;
        };
        let destination = destination_root.join(relative);
        let volume = volume_key(&destination);
        request.read_volumes.push(volume.clone());
        request.write_volumes.push(volume);
        request.mutation_paths.push(path_key(&destination));
        accumulate_storage(&mut destination_totals, &destination, entry.size);
    }
    for (_, (path, bytes)) in destination_totals {
        reserve_storage(request, &path, bytes);
    }
}

fn reserve_storage(request: &mut ResourceRequest, path: &Path, bytes: u64) {
    if bytes == 0 {
        return;
    }
    request.storage_reservations.push(StorageReservation {
        volume: volume_key(path),
        probe_path: path.to_path_buf(),
        bytes,
    });
}

fn reserve_remaining_file(request: &mut ResourceRequest, path: &Path, expected_size: u64) {
    reserve_storage(
        request,
        path,
        expected_size.saturating_sub(existing_file_len(path).min(expected_size)),
    );
}

fn accumulate_storage(totals: &mut BTreeMap<String, (PathBuf, u64)>, path: &Path, bytes: u64) {
    if bytes == 0 {
        return;
    }
    let volume = volume_key(path);
    let entry = totals
        .entry(volume)
        .or_insert_with(|| (path.to_path_buf(), 0));
    entry.1 = entry.1.saturating_add(bytes);
}

fn existing_file_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn task_estimated_bytes(task: &Task) -> u64 {
    match task {
        Task::FetchArchiveRange { request, .. } => {
            request.local_range.end - request.local_range.start
        }
        Task::FetchArchiveRepairFile { repair } => repair.source_bytes,
        Task::ExtractArchiveRepairFile { repair } => repair.expected_size,
        Task::Download { expected_size, .. } | Task::Verify { expected_size, .. } => {
            expected_size.unwrap_or(0)
        }
        Task::RepairFile { expected_size, .. }
        | Task::VerifyReuseVolume { expected_size, .. }
        | Task::ReuseFile { expected_size, .. } => *expected_size,
        Task::ProbePatchArtifact {
            patch_check,
            probe_index,
        } => patch_check.probe_size(*probe_index).unwrap_or(0),
        Task::ExtractArchiveShard { shard } => shard.estimated_cost,
        Task::ApplyPatchEntry { patch, entry_index } => patch
            .entry(*entry_index)
            .map(|entry| entry.expected_size)
            .unwrap_or(0),
        Task::RetainArchiveVolume { work, volume_index } => work
            .parts
            .get(*volume_index)
            .map(|part| part.expected_size)
            .unwrap_or(0),
        Task::OpenArchive { source, .. } => match source {
            ArchiveSource::Remote(parts) => parts
                .iter()
                .filter_map(|part| std::fs::metadata(&part.dest).ok())
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len())
                .sum(),
            ArchiveSource::Local(volumes) => volumes
                .iter()
                .filter_map(|path| std::fs::metadata(path).ok())
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len())
                .sum(),
        },
        Task::DiscoverArchiveDirectory { .. }
        | Task::InspectArchiveIndex { .. }
        | Task::ReadArchiveControls { .. }
        | Task::MeasurePatchRelocation { .. }
        | Task::SavePatchPlan { .. }
        | Task::FinishArchive { .. }
        | Task::PreparePatchApply { .. }
        | Task::ReleasePatchBase { .. }
        | Task::ApplyPatchDeletes { .. }
        | Task::CommitPatchDeferred { .. }
        | Task::CleanPatchApply { .. }
        | Task::CleanupArchive { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. } => 0,
    }
}

fn run_class(task: &Task) -> RunClass {
    match task {
        Task::Download { resume, .. } => {
            if resume.is_some() {
                RunClass::AsyncIo
            } else {
                RunClass::Cpu
            }
        }
        Task::FetchArchiveRepairFile { .. }
        | Task::FetchArchiveRange { .. }
        | Task::ReuseFile {
            copy_only: true, ..
        }
        | Task::ApplyDeleteManifest { .. } => RunClass::AsyncIo,
        Task::Verify { .. }
        | Task::ReuseFile {
            copy_only: false, ..
        }
        | Task::RepairFile { .. }
        | Task::VerifyReuseVolume { .. }
        | Task::ProbePatchArtifact { .. } => RunClass::Cpu,
        Task::ApplyPatchEntry { patch, entry_index } => patch
            .entry(*entry_index)
            .map(|entry| match &entry.source {
                crate::runtime::PlannedPatchSource::Local { .. } => RunClass::Blocking,
                crate::runtime::PlannedPatchSource::AlreadyPresent
                | crate::runtime::PlannedPatchSource::Hdiff { .. } => RunClass::Cpu,
            })
            .unwrap_or(RunClass::Blocking),
        Task::ExtractArchiveRepairFile { .. }
        | Task::OpenArchive { .. }
        | Task::DiscoverArchiveDirectory { .. }
        | Task::InspectArchiveIndex { .. }
        | Task::ReadArchiveControls { .. }
        | Task::MeasurePatchRelocation { .. }
        | Task::SavePatchPlan { .. }
        | Task::ExtractArchiveShard { .. }
        | Task::RetainArchiveVolume { .. }
        | Task::FinishArchive { .. }
        | Task::PreparePatchApply { .. }
        | Task::ReleasePatchBase { .. }
        | Task::ApplyPatchDeletes { .. }
        | Task::CommitPatchDeferred { .. }
        | Task::CleanPatchApply { .. }
        | Task::CleanupArchive { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. } => RunClass::Blocking,
    }
}

fn normalize_volumes(request: &mut ResourceRequest) {
    let writes = request.write_volumes.drain(..).collect::<BTreeSet<_>>();
    let metadata = request
        .metadata_volumes
        .drain(..)
        .filter(|volume| !writes.contains(volume))
        .collect::<BTreeSet<_>>();
    let reads = request.read_volumes.drain(..).collect::<BTreeSet<_>>();
    let savers = request
        .archive_save_volumes
        .drain(..)
        .collect::<BTreeSet<_>>();
    let commits = request
        .archive_commit_volumes
        .drain(..)
        .collect::<BTreeSet<_>>();
    let mutations = request.mutation_paths.drain(..).collect::<BTreeSet<_>>();
    let mut storage = BTreeMap::<String, (PathBuf, u64)>::new();
    for reservation in request.storage_reservations.drain(..) {
        let entry = storage
            .entry(reservation.volume)
            .or_insert((reservation.probe_path, 0));
        entry.1 = entry.1.saturating_add(reservation.bytes);
    }
    request.write_volumes.extend(writes);
    request.metadata_volumes.extend(metadata);
    request.read_volumes.extend(reads);
    request.archive_save_volumes.extend(savers);
    request.archive_commit_volumes.extend(commits);
    request.mutation_paths.extend(mutations);
    request.storage_reservations.extend(storage.into_iter().map(
        |(volume, (probe_path, bytes))| StorageReservation {
            volume,
            probe_path,
            bytes,
        },
    ));
}

fn physical_patch_path(plan: &crate::runtime::PatchPlan, relative: &Path) -> std::path::PathBuf {
    let logical_vfs_root = plan.install_root.join(&plan.vfs_base_path);
    if plan.vfs_destination != logical_vfs_root {
        if let Ok(vfs_relative) = relative.strip_prefix(&plan.vfs_base_path) {
            return plan.vfs_destination.join(vfs_relative);
        }
    }
    plan.install_root.join(relative)
}

fn volume_key(path: &Path) -> String {
    crate::runtime::task_pool::fs_ops::storage_volume_group_key(path)
}

fn path_key(path: &Path) -> String {
    crate::runtime::artifact::physical_path_key(path)
}

pub(super) fn task_path(task: &Task) -> String {
    match task {
        Task::FetchArchiveRepairFile { repair } | Task::ExtractArchiveRepairFile { repair } => {
            repair.logical_path.clone()
        }
        Task::OpenArchive { base_name, .. } => base_name.clone(),
        Task::ProbePatchArtifact {
            patch_check,
            probe_index,
        } => patch_check
            .probe_path(*probe_index)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("patch-probe-{probe_index}")),
        Task::MeasurePatchRelocation { patch_check } => patch_check
            .relocation_root()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "patch-relocation".to_string()),
        Task::DiscoverArchiveDirectory { work, .. }
        | Task::InspectArchiveIndex { work, .. }
        | Task::ReadArchiveControls { work, .. }
        | Task::SavePatchPlan { work, .. }
        | Task::RetainArchiveVolume { work, .. }
        | Task::FinishArchive { work }
        | Task::CleanupArchive { work } => work.base_name.clone(),
        Task::ExtractArchiveShard { shard } => shard.work.base_name.clone(),
        Task::FetchArchiveRange { work, request, .. } => format!(
            "{}#volume-{:03}:{}-{}",
            work.base_name,
            request.volume_index + 1,
            request.local_range.start,
            request.local_range.end
        ),
        Task::Download { logical_path, .. }
        | Task::Verify { logical_path, .. }
        | Task::RepairFile { logical_path, .. }
        | Task::VerifyReuseVolume { logical_path, .. }
        | Task::ReuseFile { logical_path, .. } => logical_path.clone(),
        Task::PreparePatchApply { patch }
        | Task::ApplyPatchDeletes { patch }
        | Task::CommitPatchDeferred { patch } => patch.plan().install_root.display().to_string(),
        Task::ApplyPatchEntry { patch, entry_index } => patch
            .entry(*entry_index)
            .map(|entry| entry.destination.display().to_string())
            .unwrap_or_else(|| format!("patch-entry-{entry_index}")),
        Task::ReleasePatchBase { base, .. } => base.display().to_string(),
        Task::CleanPatchApply { patch, .. } => patch.plan().stage_root.display().to_string(),
        Task::ApplyExtractedVfsPatchManifest { install_root }
        | Task::ApplyDeleteManifest { install_root } => install_root.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_volumes, patch_prepare_commit_files, route_archive_range_write, task_resources,
        NetworkClass, ResourceRequest, RunClass, StorageReservation,
    };
    use crate::download::extractor::ArchiveRangeRequest;
    use crate::runtime::task_pool::types::{ArchiveRepairSession, DownloadResumeState};
    use crate::runtime::task_pool::{Task, TransferClass};
    use crate::runtime::{griffr_patch_path, PatchPlan, PATCH_DEFERRED_DIR, PATCH_STAGE_DIR};
    use md5::{Digest, Md5};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn mutation_path_keys_preserve_case_on_case_sensitive_platforms() {
        assert_ne!(
            super::path_key(Path::new("Data")),
            super::path_key(Path::new("data"))
        );
    }

    fn reuse_task(copy_only: bool) -> Task {
        Task::ReuseFile {
            source: PathBuf::from("volume-a/source.bin"),
            copy_only,
            remaining_source_candidates: Vec::new(),
            dest: PathBuf::from("volume-a/dest.bin"),
            logical_path: "volume-a/dest.bin".to_string(),
            expected_md5: "00000000000000000000000000000000".to_string(),
            expected_size: 1,
            download_url: None,
            allow_copy_fallback: true,
            verify_destination_fallback: false,
            retry_count: 0,
            transfer_class: TransferClass::General,
            archive_repair: None,
        }
    }

    #[test]
    fn hardlink_reuse_accounts_for_hashing_and_metadata_capacity() {
        let resources = task_resources(&reuse_task(false));
        assert_eq!(resources.read_volumes.len(), 1);
        assert!(resources.write_volumes.is_empty());
        assert_eq!(resources.metadata_volumes.len(), 1);
        assert!(resources.reuse_commit);
        assert_eq!(resources.estimated_bytes, 1);
        assert_eq!(resources.run, RunClass::Cpu);
    }

    #[test]
    fn copy_reuse_preserves_same_volume_read_and_write_pressure() {
        let resources = task_resources(&reuse_task(true));
        assert_eq!(resources.read_volumes.len(), 1);
        assert_eq!(resources.write_volumes.len(), 1);
        assert_eq!(resources.read_volumes, resources.write_volumes);
        assert!(resources.metadata_volumes.is_empty());
        assert_eq!(resources.storage_reservations.len(), 1);
        assert_eq!(resources.storage_reservations[0].bytes, 1);
        assert!(!resources.reuse_commit);
        assert_eq!(resources.run, RunClass::AsyncIo);
    }

    #[test]
    fn delete_manifest_uses_async_dispatcher_runtime() {
        let resources = task_resources(&Task::ApplyDeleteManifest {
            install_root: PathBuf::from("game"),
        });
        assert_eq!(resources.run, RunClass::AsyncIo);
    }

    #[test]
    fn reuse_hardlink_uses_cpu_dispatcher_runtime() {
        let reuse = task_resources(&reuse_task(false));
        assert_eq!(reuse.run, RunClass::Cpu);
    }

    #[test]
    fn repair_route_waits_for_its_network_lane_before_source_selection() {
        let resources = task_resources(&Task::Download {
            url: "https://example.invalid/file.bin".to_string(),
            dest: PathBuf::from("game/file.bin"),
            logical_path: "file.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: Some(4),
            retry_count: 0,
            transfer_class: TransferClass::General,
            archive_repair: Some(ArchiveRepairSession::new(
                Vec::new(),
                PathBuf::from("game"),
                Arc::new(BTreeMap::new()),
            )),
            resume: Some(DownloadResumeState::new(0, Md5::new())),
        });

        assert_eq!(resources.run, RunClass::AsyncIo);
        assert_eq!(resources.network, Some(NetworkClass::General));
        assert_eq!(resources.estimated_bytes, 4);
        assert_eq!(resources.storage_reservations.len(), 1);
        assert_eq!(resources.storage_reservations[0].bytes, 4);
    }

    #[test]
    fn archive_range_reserves_only_the_partial_file_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let cache_path = temp.path().join("v0000-0-10.range");
        let part_path = cache_path.with_extension("range.part");
        std::fs::write(&part_path, [0u8; 4]).unwrap();
        let range = ArchiveRangeRequest {
            volume_index: 0,
            local_range: 0..10,
            global_range: 0..10,
            url: "https://example.invalid/archive.zip.001".to_string(),
            cache_path,
        };
        let mut resources = ResourceRequest::default();

        route_archive_range_write(&mut resources, &range);
        normalize_volumes(&mut resources);

        assert_eq!(resources.storage_reservations.len(), 1);
        assert_eq!(resources.storage_reservations[0].bytes, 6);
        assert_eq!(resources.mutation_paths.len(), 2);
    }

    #[test]
    fn resumed_download_reserves_only_the_missing_suffix() {
        let resources = task_resources(&Task::Download {
            url: "https://example.invalid/file.bin".to_string(),
            dest: PathBuf::from("game/file.bin"),
            logical_path: "file.bin".to_string(),
            expected_md5: "00".repeat(16),
            expected_size: Some(10),
            retry_count: 0,
            transfer_class: TransferClass::General,
            archive_repair: None,
            resume: Some(DownloadResumeState::new(4, Md5::new())),
        });

        assert_eq!(resources.storage_reservations.len(), 1);
        assert_eq!(resources.storage_reservations[0].bytes, 6);
    }

    #[test]
    fn simultaneous_reservations_on_one_volume_are_combined() {
        let mut resources = ResourceRequest {
            storage_reservations: vec![
                StorageReservation {
                    volume: "volume-a".to_string(),
                    probe_path: PathBuf::from("a.bin"),
                    bytes: 4,
                },
                StorageReservation {
                    volume: "volume-a".to_string(),
                    probe_path: PathBuf::from("b.bin"),
                    bytes: 6,
                },
            ],
            ..ResourceRequest::default()
        };

        normalize_volumes(&mut resources);

        assert_eq!(resources.storage_reservations.len(), 1);
        assert_eq!(resources.storage_reservations[0].bytes, 10);
    }

    #[test]
    fn patch_prepare_collects_top_level_and_deferred_files_only() {
        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("install");
        let stage_root = temp.path().join("stage");
        std::fs::create_dir_all(stage_root.join(PATCH_STAGE_DIR)).unwrap();
        std::fs::write(stage_root.join("top.bin"), [0u8; 4]).unwrap();
        std::fs::write(stage_root.join("config.ini"), [0u8; 7]).unwrap();
        std::fs::write(
            stage_root.join(PATCH_STAGE_DIR).join("payload.bin"),
            [0u8; 9],
        )
        .unwrap();
        std::fs::write(stage_root.join(crate::runtime::PATCH_MANIFEST_NAME), b"{}").unwrap();

        let plan = PatchPlan {
            schema_version: PatchPlan::SCHEMA_VERSION,
            install_root: install_root.clone(),
            stage_root,
            vfs_base_path: PathBuf::from("Data/VFS"),
            vfs_destination: install_root.join("Data/VFS"),
            work_dir: None,
            entries: Vec::new(),
            delete_paths: Vec::new(),
            deferred_paths: vec![PathBuf::from("config.ini")],
        };
        let files = patch_prepare_commit_files(&plan)
            .into_iter()
            .map(|(_, destination, bytes)| (destination, bytes))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(files.len(), 2);
        assert_eq!(files.get(&install_root.join("top.bin")), Some(&4));
        assert_eq!(
            files.get(
                &griffr_patch_path(&install_root)
                    .join(PATCH_DEFERRED_DIR)
                    .join("config.ini")
            ),
            Some(&7)
        );
    }

    #[test]
    fn legacy_vfs_patch_reserves_all_outputs_created_by_one_task() {
        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("install");
        std::fs::create_dir_all(&install_root).unwrap();
        std::fs::write(
            install_root.join(crate::runtime::PATCH_MANIFEST_NAME),
            r#"{
  "version": "1",
  "vfs_base_path": "Data/VFS",
  "files": [
    {"name":"a.bin","md5":"00","size":4,"diffType":0,"patch":[]},
    {"name":"b.bin","md5":"00","size":9,"diffType":0,"patch":[]}
  ]
}"#,
        )
        .unwrap();

        let resources = task_resources(&Task::ApplyExtractedVfsPatchManifest { install_root });

        assert_eq!(resources.storage_reservations.len(), 1);
        assert_eq!(resources.storage_reservations[0].bytes, 13);
    }
}

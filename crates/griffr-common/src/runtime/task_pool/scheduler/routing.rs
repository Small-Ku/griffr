use std::collections::BTreeSet;
use std::path::Path;

use crate::runtime::task_pool::{ArchiveRangePriority, Task, TransferClass};

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
            request.write_volumes.push(volume_key(&range.cache_path));
            request.mutation_paths.push(path_key(&range.cache_path));
        }
        Task::Verify { path, .. } => request.read_volumes.push(volume_key(path)),
        Task::Download {
            dest,
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
            } else {
                request.read_volumes.push(volume.clone());
                request.metadata_volumes.push(volume);
            }
            request.mutation_paths.push(path_key(dest));
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
            ..
        } => {
            if *copy_only {
                request.read_volumes.push(volume_key(source));
                request.write_volumes.push(volume_key(dest));
                request.mutation_paths.push(path_key(dest));
            } else {
                request.metadata_volumes.push(volume_key(dest));
                request.mutation_paths.push(path_key(dest));
                request.reuse_commit = true;
            }
        }
        Task::Extract { .. } => {}
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
        }
        Task::PlanArchiveExtraction { work, .. } => {
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
            request.write_volumes.push(volume_key(&shard.staging_dir));
            request.extract = true;
        }
        Task::FillArchiveVolumeGaps { work, volume_index } => {
            if let Some(path) = work.layout.path(*volume_index) {
                request.metadata_volumes.push(volume_key(path));
            }
        }
        Task::SaveArchiveVolume { work, volume_index } => {
            if let Some(path) = work.layout.path(*volume_index) {
                request.read_volumes.push(volume_key(path));
            }
            if let Some(part) = work.parts.get(*volume_index) {
                let volume = volume_key(&part.dest);
                request.write_volumes.push(volume.clone());
                request.archive_save_volumes.push(volume);
                request.mutation_paths.push(path_key(&part.dest));
            }
        }
        Task::ArchiveVolumesReady { .. } => {}
        Task::CommitArchive { work } => {
            if let Some(prepared) = work.prepared.lock().unwrap().as_ref() {
                request
                    .metadata_volumes
                    .push(volume_key(&prepared.staging_dir));
            }
        }
        Task::CommitArchiveBatch {
            commit,
            batch_index,
        } => {
            if let Some(batch) = commit.batch(*batch_index) {
                for job in &batch.jobs {
                    let destination_volume = volume_key(&job.destination);
                    if batch.cross_volume {
                        request.read_volumes.push(volume_key(&job.source));
                        request.write_volumes.push(destination_volume.clone());
                    } else {
                        request.metadata_volumes.push(destination_volume.clone());
                    }
                    request
                        .archive_commit_volumes
                        .push((destination_volume, batch.cross_volume));
                    request.mutation_paths.push(path_key(&job.destination));
                }
            }
        }
        Task::VerifyCommittedBatch {
            commit,
            batch_index,
        } => {
            if let Some(batch) = commit.batch(*batch_index) {
                request
                    .read_volumes
                    .extend(batch.jobs.iter().map(|job| volume_key(&job.destination)));
            }
        }
        Task::FinishArchiveCommit { commit } => {
            request
                .metadata_volumes
                .push(volume_key(&commit.staging_dir));
            request.mutation_paths.push(path_key(&commit.staging_dir));
        }
        Task::PreparePatchApply { patch } => {
            let plan = patch.plan();
            request.read_volumes.push(volume_key(&plan.stage_root));
            request.write_volumes.push(volume_key(&plan.install_root));
            request
                .write_volumes
                .push(volume_key(&plan.vfs_destination));
            if let Some(work_dir) = plan.work_dir.as_deref() {
                request.write_volumes.push(volume_key(work_dir));
            }
            request.mutation_paths.push(path_key(&plan.install_root));
        }
        Task::ApplyPatchEntry { patch, entry_index } => {
            if let Some(entry) = patch.entry(*entry_index) {
                match &entry.source {
                    crate::runtime::PlannedPatchSource::AlreadyPresent => {
                        request.read_volumes.push(volume_key(&entry.destination));
                    }
                    crate::runtime::PlannedPatchSource::Local { payload } => {
                        request
                            .read_volumes
                            .push(volume_key(&patch.plan().stage_root.join(payload)));
                        request.write_volumes.push(volume_key(&entry.destination));
                    }
                    crate::runtime::PlannedPatchSource::Hdiff { base, payload, .. } => {
                        request.read_volumes.push(volume_key(base));
                        request
                            .read_volumes
                            .push(volume_key(&patch.plan().stage_root.join(payload)));
                        request.write_volumes.push(volume_key(&entry.destination));
                        if let Some(work_dir) = patch.plan().work_dir.as_deref() {
                            request.write_volumes.push(volume_key(work_dir));
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
            let deferred_root = plan
                .install_root
                .join(crate::runtime::PATCH_WORK_DIR)
                .join(crate::runtime::PATCH_DEFERRED_DIR);
            for relative in &plan.deferred_paths {
                request
                    .read_volumes
                    .push(volume_key(&deferred_root.join(relative)));
                let target = plan.install_root.join(relative);
                request.write_volumes.push(volume_key(&target));
                request.mutation_paths.push(path_key(&target));
            }
        }
        Task::CleanPatchApply { patch, archive: _ } => {
            let plan = patch.plan();
            let patch_root = plan.install_root.join(crate::runtime::PATCH_WORK_DIR);
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
            let volume = volume_key(install_root);
            request.read_volumes.push(volume.clone());
            request.write_volumes.push(volume);
            request.mutation_paths.push(path_key(install_root));
        }
        Task::ApplyDeleteManifest { install_root } => {
            request.metadata_volumes.push(volume_key(install_root));
            request.mutation_paths.push(path_key(install_root));
        }
        Task::Hardlink { dest, .. } => {
            request.metadata_volumes.push(volume_key(dest));
            request.mutation_paths.push(path_key(dest));
        }
        Task::InstallArchive { .. } | Task::RepairFile { .. } => {}
    }
    request.estimated_bytes = task_estimated_bytes(task);
    normalize_volumes(&mut request);
    request
}

fn task_estimated_bytes(task: &Task) -> u64 {
    match task {
        Task::FetchArchiveRange { request, .. } => {
            request.local_range.end - request.local_range.start
        }
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
        Task::CommitArchiveBatch {
            commit,
            batch_index,
        }
        | Task::VerifyCommittedBatch {
            commit,
            batch_index,
        } => commit
            .batch(*batch_index)
            .map(|batch| batch.estimated_bytes)
            .unwrap_or(0),
        Task::ApplyPatchEntry { patch, entry_index } => patch
            .entry(*entry_index)
            .map(|entry| entry.expected_size)
            .unwrap_or(0),
        Task::SaveArchiveVolume { work, volume_index } => work
            .parts
            .get(*volume_index)
            .map(|part| part.expected_size)
            .unwrap_or(0),
        Task::InstallArchive { .. }
        | Task::Extract { .. }
        | Task::DiscoverArchiveDirectory { .. }
        | Task::InspectArchiveIndex { .. }
        | Task::ReadArchiveControls { .. }
        | Task::PlanArchiveExtraction { .. }
        | Task::MeasurePatchRelocation { .. }
        | Task::SavePatchPlan { .. }
        | Task::FillArchiveVolumeGaps { .. }
        | Task::ArchiveVolumesReady { .. }
        | Task::CommitArchive { .. }
        | Task::FinishArchiveCommit { .. }
        | Task::PreparePatchApply { .. }
        | Task::ReleasePatchBase { .. }
        | Task::ApplyPatchDeletes { .. }
        | Task::CommitPatchDeferred { .. }
        | Task::CleanPatchApply { .. }
        | Task::CleanupArchive { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. }
        | Task::Hardlink { .. } => 0,
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
        Task::FetchArchiveRange { .. }
        | Task::Hardlink { .. }
        | Task::ReuseFile { .. }
        | Task::ApplyDeleteManifest { .. } => RunClass::AsyncIo,
        Task::Verify { .. }
        | Task::RepairFile { .. }
        | Task::VerifyReuseVolume { .. }
        | Task::ProbePatchArtifact { .. }
        | Task::VerifyCommittedBatch { .. } => RunClass::Cpu,
        Task::ApplyPatchEntry { patch, entry_index } => patch
            .entry(*entry_index)
            .map(|entry| match &entry.source {
                crate::runtime::PlannedPatchSource::Local { .. } => RunClass::Blocking,
                crate::runtime::PlannedPatchSource::AlreadyPresent
                | crate::runtime::PlannedPatchSource::Hdiff { .. } => RunClass::Cpu,
            })
            .unwrap_or(RunClass::Blocking),
        Task::InstallArchive { .. }
        | Task::Extract { .. }
        | Task::DiscoverArchiveDirectory { .. }
        | Task::InspectArchiveIndex { .. }
        | Task::ReadArchiveControls { .. }
        | Task::PlanArchiveExtraction { .. }
        | Task::MeasurePatchRelocation { .. }
        | Task::SavePatchPlan { .. }
        | Task::ExtractArchiveShard { .. }
        | Task::FillArchiveVolumeGaps { .. }
        | Task::SaveArchiveVolume { .. }
        | Task::ArchiveVolumesReady { .. }
        | Task::CommitArchive { .. }
        | Task::CommitArchiveBatch { .. }
        | Task::FinishArchiveCommit { .. }
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
    request.write_volumes.extend(writes);
    request.metadata_volumes.extend(metadata);
    request.read_volumes.extend(reads);
    request.archive_save_volumes.extend(savers);
    request.archive_commit_volumes.extend(commits);
    request.mutation_paths.extend(mutations);
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
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

pub(super) fn task_path(task: &Task) -> String {
    match task {
        Task::InstallArchive { base_name, .. } | Task::Extract { base_name, .. } => {
            base_name.clone()
        }
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
        | Task::PlanArchiveExtraction { work, .. }
        | Task::SavePatchPlan { work, .. }
        | Task::FillArchiveVolumeGaps { work, .. }
        | Task::SaveArchiveVolume { work, .. }
        | Task::ArchiveVolumesReady { work }
        | Task::CommitArchive { work }
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
        Task::CommitArchiveBatch {
            commit,
            batch_index,
        }
        | Task::VerifyCommittedBatch {
            commit,
            batch_index,
        } => format!("{}#commit-batch-{batch_index}", commit.archive.base_name),
        Task::FinishArchiveCommit { commit } => commit.archive.base_name.clone(),
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
        Task::Hardlink { dest, .. } => dest.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{task_resources, RunClass};
    use crate::runtime::task_pool::{Task, TransferClass};
    use std::path::PathBuf;

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
        }
    }

    #[test]
    fn hardlink_reuse_uses_metadata_capacity_without_streaming_read() {
        let resources = task_resources(&reuse_task(false));
        assert!(resources.read_volumes.is_empty());
        assert!(resources.write_volumes.is_empty());
        assert_eq!(resources.metadata_volumes.len(), 1);
        assert!(resources.reuse_commit);
        assert_eq!(resources.run, RunClass::AsyncIo);
    }

    #[test]
    fn copy_reuse_preserves_same_volume_read_and_write_pressure() {
        let resources = task_resources(&reuse_task(true));
        assert_eq!(resources.read_volumes.len(), 1);
        assert_eq!(resources.write_volumes.len(), 1);
        assert_eq!(resources.read_volumes, resources.write_volumes);
        assert!(resources.metadata_volumes.is_empty());
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
    fn hardlink_mutations_use_async_dispatcher_runtime() {
        let reuse = task_resources(&reuse_task(false));
        assert_eq!(reuse.run, RunClass::AsyncIo);

        let hardlink = task_resources(&Task::Hardlink {
            src: PathBuf::from("source.bin"),
            dest: PathBuf::from("dest.bin"),
        });
        assert_eq!(hardlink.run, RunClass::AsyncIo);
    }
}

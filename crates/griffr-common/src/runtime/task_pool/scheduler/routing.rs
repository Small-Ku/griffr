use std::collections::BTreeSet;
use std::path::Path;

use crate::runtime::task_pool::{Task, TransferClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExecutionClass {
    AsyncIo,
    Cpu,
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NetworkClass {
    General,
    Vfs,
    Archive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResourceRequest {
    pub(super) execution: ExecutionClass,
    pub(super) network: Option<NetworkClass>,
    pub(super) read_volumes: Vec<String>,
    pub(super) write_volumes: Vec<String>,
    pub(super) metadata_volumes: Vec<String>,
    pub(super) extract: bool,
    pub(super) mutation_root: Option<String>,
    pub(super) estimated_bytes: u64,
    pub(super) reuse_probe: bool,
    pub(super) reuse_commit: bool,
}

impl Default for ResourceRequest {
    fn default() -> Self {
        Self {
            execution: ExecutionClass::Blocking,
            network: None,
            read_volumes: Vec::new(),
            write_volumes: Vec::new(),
            metadata_volumes: Vec::new(),
            extract: false,
            mutation_root: None,
            estimated_bytes: 0,
            reuse_probe: false,
            reuse_commit: false,
        }
    }
}

pub(super) fn task_resources(task: &Task) -> ResourceRequest {
    let mut request = ResourceRequest {
        execution: execution_class(task),
        ..ResourceRequest::default()
    };
    match task {
        Task::TransferDownload {
            dest,
            transfer_class,
            ..
        } => {
            request.network = Some(match transfer_class {
                TransferClass::General => NetworkClass::General,
                TransferClass::Vfs => NetworkClass::Vfs,
            });
            request.write_volumes.push(volume_key(dest));
            request.mutation_root = Some(path_key(dest));
        }
        Task::TransferArchivePart { part, .. } => {
            request.network = Some(NetworkClass::Archive);
            request.write_volumes.push(volume_key(&part.dest));
            request.mutation_root = Some(path_key(&part.dest));
        }
        Task::Verify { path, .. } => request.read_volumes.push(volume_key(path)),
        Task::Download { dest, .. } => {
            let volume = volume_key(dest);
            request.read_volumes.push(volume.clone());
            request.metadata_volumes.push(volume);
            request.mutation_root = Some(path_key(dest));
        }
        Task::InstallArchivePart { part, .. } => {
            let volume = volume_key(&part.dest);
            request.read_volumes.push(volume.clone());
            request.metadata_volumes.push(volume);
            request.mutation_root = Some(path_key(&part.dest));
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
                request.mutation_root = Some(path_key(dest));
            } else {
                request.metadata_volumes.push(volume_key(dest));
                request.mutation_root = Some(path_key(dest));
                request.reuse_commit = true;
            }
        }
        Task::Extract { .. } => {}
        Task::PrepareArchive { work } => {
            request
                .read_volumes
                .extend(work.volumes.iter().map(|path| volume_key(path)));
        }
        Task::ExtractArchiveShard { shard } => {
            request
                .read_volumes
                .extend(shard.work.volumes.iter().map(|path| volume_key(path)));
            request.write_volumes.push(volume_key(&shard.staging_dir));
            request.extract = true;
        }
        Task::CommitArchive { work } => {
            request.write_volumes.push(volume_key(&work.dest));
            let prepared = work.prepared.lock().unwrap();
            if let Some(prepared) = prepared.as_ref() {
                request.read_volumes.push(volume_key(&prepared.staging_dir));
                if let Some((plan, _)) = prepared.patch_plan.as_ref() {
                    request
                        .write_volumes
                        .push(volume_key(&plan.vfs_destination));
                    if let Some(work_dir) = plan.work_dir.as_deref() {
                        request.write_volumes.push(volume_key(work_dir));
                    }
                }
            }
            request.mutation_root = Some(path_key(&work.dest));
        }
        Task::CleanupArchive { work } => {
            request
                .metadata_volumes
                .extend(work.volumes.iter().map(|path| volume_key(path)));
        }
        Task::ApplyExtractedVfsPatchManifest { install_root } => {
            let volume = volume_key(install_root);
            request.read_volumes.push(volume.clone());
            request.write_volumes.push(volume);
            request.mutation_root = Some(path_key(install_root));
        }
        Task::ApplyDeleteManifest { install_root } => {
            request.metadata_volumes.push(volume_key(install_root));
            request.mutation_root = Some(path_key(install_root));
        }
        Task::Hardlink { dest, .. } => {
            request.metadata_volumes.push(volume_key(dest));
            request.mutation_root = Some(path_key(dest));
        }
        Task::InstallArchive { .. } | Task::RepairFile { .. } => {}
    }
    request.estimated_bytes = task_estimated_bytes(task);
    normalize_volumes(&mut request);
    request
}

fn task_estimated_bytes(task: &Task) -> u64 {
    match task {
        Task::InstallArchivePart { part, .. } | Task::TransferArchivePart { part, .. } => {
            part.expected_size
        }
        Task::Download { expected_size, .. }
        | Task::TransferDownload { expected_size, .. }
        | Task::Verify { expected_size, .. } => expected_size.unwrap_or(0),
        Task::RepairFile { expected_size, .. }
        | Task::VerifyReuseVolume { expected_size, .. }
        | Task::ReuseFile { expected_size, .. } => *expected_size,
        Task::ExtractArchiveShard { shard } => {
            crate::download::extractor::MultiVolumeExtractor::range_uncompressed_bytes(
                &shard.inspection,
                shard.range.clone(),
            )
        }
        Task::InstallArchive { .. }
        | Task::Extract { .. }
        | Task::PrepareArchive { .. }
        | Task::CommitArchive { .. }
        | Task::CleanupArchive { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. }
        | Task::Hardlink { .. } => 0,
    }
}

fn execution_class(task: &Task) -> ExecutionClass {
    match task {
        Task::TransferDownload { .. }
        | Task::TransferArchivePart { .. }
        | Task::Hardlink { .. }
        | Task::ReuseFile {
            copy_only: false, ..
        } => ExecutionClass::AsyncIo,
        Task::InstallArchivePart { .. }
        | Task::Download { .. }
        | Task::Verify { .. }
        | Task::RepairFile { .. }
        | Task::VerifyReuseVolume { .. } => ExecutionClass::Cpu,
        Task::InstallArchive { .. }
        | Task::ReuseFile {
            copy_only: true, ..
        }
        | Task::Extract { .. }
        | Task::PrepareArchive { .. }
        | Task::ExtractArchiveShard { .. }
        | Task::CommitArchive { .. }
        | Task::CleanupArchive { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. } => ExecutionClass::Blocking,
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
    request.write_volumes.extend(writes);
    request.metadata_volumes.extend(metadata);
    request.read_volumes.extend(reads);
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
        Task::PrepareArchive { work }
        | Task::CommitArchive { work }
        | Task::CleanupArchive { work } => work.base_name.clone(),
        Task::ExtractArchiveShard { shard } => shard.work.base_name.clone(),
        Task::InstallArchivePart { part, .. } | Task::TransferArchivePart { part, .. } => {
            part.logical_path.clone()
        }
        Task::Download { logical_path, .. }
        | Task::TransferDownload { logical_path, .. }
        | Task::Verify { logical_path, .. }
        | Task::RepairFile { logical_path, .. }
        | Task::VerifyReuseVolume { logical_path, .. }
        | Task::ReuseFile { logical_path, .. } => logical_path.clone(),
        Task::ApplyExtractedVfsPatchManifest { install_root }
        | Task::ApplyDeleteManifest { install_root } => install_root.display().to_string(),
        Task::Hardlink { dest, .. } => dest.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{task_resources, ExecutionClass};
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
        assert_eq!(resources.execution, ExecutionClass::AsyncIo);
    }

    #[test]
    fn copy_reuse_preserves_same_volume_read_and_write_pressure() {
        let resources = task_resources(&reuse_task(true));
        assert_eq!(resources.read_volumes.len(), 1);
        assert_eq!(resources.write_volumes.len(), 1);
        assert_eq!(resources.read_volumes, resources.write_volumes);
        assert!(resources.metadata_volumes.is_empty());
        assert!(!resources.reuse_commit);
        assert_eq!(resources.execution, ExecutionClass::Blocking);
    }

    #[test]
    fn hardlink_mutations_use_async_dispatcher_runtime() {
        let reuse = task_resources(&reuse_task(false));
        assert_eq!(reuse.execution, ExecutionClass::AsyncIo);

        let hardlink = task_resources(&Task::Hardlink {
            src: PathBuf::from("source.bin"),
            dest: PathBuf::from("dest.bin"),
        });
        assert_eq!(hardlink.execution, ExecutionClass::AsyncIo);
    }
}

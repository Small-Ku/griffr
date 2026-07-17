use std::collections::BTreeSet;
use std::path::{Component, Path};

use crate::runtime::task_pool::{Task, TaskPoolConfig, TransferClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExecutionClass {
    Network,
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
    pub(super) extract: bool,
    pub(super) mutation_root: Option<String>,
}

impl Default for ResourceRequest {
    fn default() -> Self {
        Self {
            execution: ExecutionClass::Blocking,
            network: None,
            read_volumes: Vec::new(),
            write_volumes: Vec::new(),
            extract: false,
            mutation_root: None,
        }
    }
}

pub(super) fn dispatcher_thread_count(config: &TaskPoolConfig) -> usize {
    config.dispatcher_threads.clamp(2, 4)
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
        }
        Task::TransferArchivePart { part, .. } => {
            request.network = Some(NetworkClass::Archive);
            request.write_volumes.push(volume_key(&part.dest));
        }
        Task::Verify { path, .. } => request.read_volumes.push(volume_key(path)),
        Task::Download { dest, .. } => {
            request.write_volumes.push(volume_key(dest));
        }
        Task::InstallArchivePart { part, .. } => {
            request.write_volumes.push(volume_key(&part.dest));
        }
        Task::VerifyReuseVolume { candidates, .. } => {
            if let Some(path) = candidates.first() {
                request.read_volumes.push(volume_key(path));
            }
        }
        Task::ReuseFile {
            source, dest, ..
        } => {
            request.read_volumes.push(volume_key(source));
            request.write_volumes.push(volume_key(dest));
        }
        Task::Extract { .. } => {}
        Task::PrepareArchive { work } => {
            request
                .read_volumes
                .extend(work.volumes.first().map(|path| volume_key(path)));
        }
        Task::ExtractArchiveShard { shard } => {
            request
                .read_volumes
                .extend(shard.work.volumes.first().map(|path| volume_key(path)));
            request.write_volumes.push(volume_key(&shard.staging_dir));
            request.extract = true;
        }
        Task::CommitArchive { work } => {
            request.write_volumes.push(volume_key(&work.dest));
            request.mutation_root = Some(path_key(&work.dest));
        }
        Task::CleanupArchive { work } => {
            request
                .write_volumes
                .extend(work.volumes.first().map(|path| volume_key(path)));
        }
        Task::ApplyExtractedVfsPatchManifest { install_root }
        | Task::ApplyDeleteManifest { install_root } => {
            let volume = volume_key(install_root);
            request.write_volumes.push(volume);
            request.mutation_root = Some(path_key(install_root));
        }
        Task::Hardlink { dest, .. } => request.write_volumes.push(volume_key(dest)),
        Task::InstallArchive { .. } | Task::RepairFile { .. } => {}
    }
    normalize_volumes(&mut request);
    request
}

fn execution_class(task: &Task) -> ExecutionClass {
    match task {
        Task::TransferDownload { .. } | Task::TransferArchivePart { .. } => {
            ExecutionClass::Network
        }
        Task::InstallArchivePart { .. }
        | Task::Download { .. }
        | Task::Verify { .. }
        | Task::RepairFile { .. }
        | Task::VerifyReuseVolume { .. } => ExecutionClass::Cpu,
        Task::InstallArchive { .. }
        | Task::ReuseFile { .. }
        | Task::Extract { .. }
        | Task::PrepareArchive { .. }
        | Task::ExtractArchiveShard { .. }
        | Task::CommitArchive { .. }
        | Task::CleanupArchive { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. }
        | Task::Hardlink { .. } => ExecutionClass::Blocking,
    }
}

fn normalize_volumes(request: &mut ResourceRequest) {
    let writes = request
        .write_volumes
        .drain(..)
        .collect::<BTreeSet<_>>();
    let reads = request
        .read_volumes
        .drain(..)
        .filter(|volume| !writes.contains(volume))
        .collect::<BTreeSet<_>>();
    request.write_volumes.extend(writes);
    request.read_volumes.extend(reads);
}

fn volume_key(path: &Path) -> String {
    crate::runtime::task_pool::fs_ops::storage_volume_id(path)
        .unwrap_or_else(|| format!("unknown:{}", path_anchor(path)))
}

fn path_anchor(path: &Path) -> String {
    path.components()
        .find_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().to_string()),
            Component::RootDir => Some("/".to_string()),
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            Component::CurDir | Component::ParentDir => None,
        })
        .unwrap_or_else(|| ".".to_string())
        .to_ascii_lowercase()
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

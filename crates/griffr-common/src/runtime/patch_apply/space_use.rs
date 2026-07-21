use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::runtime::task_pool::fs_ops::storage_volume_group_key;
use crate::runtime::{DELETE_FILES_MANIFEST_NAME, PATCH_MANIFEST_NAME, PATCH_STAGE_DIR};

use super::{entry_wave_indices, PatchPlan, PlannedPatchSource};

fn normalized_archive_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_patch_archive_control_path(path: &Path) -> bool {
    path == Path::new(PATCH_MANIFEST_NAME)
        || path == Path::new(DELETE_FILES_MANIFEST_NAME)
        || path.starts_with(PATCH_STAGE_DIR)
}

fn metadata_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[derive(Debug, Default)]
struct VolumeSpaceLedger {
    current: BTreeMap<String, i128>,
    peak: BTreeMap<String, u64>,
}

impl VolumeSpaceLedger {
    fn adjust(&mut self, volume: &str, delta: i128) {
        if delta == 0 {
            return;
        }
        let current = self.current.entry(volume.to_string()).or_default();
        *current += delta;
        let positive = (*current).max(0) as u64;
        let peak = self.peak.entry(volume.to_string()).or_default();
        *peak = (*peak).max(positive);
    }

    fn add(&mut self, volume: &str, bytes: u64) {
        self.adjust(volume, i128::from(bytes));
    }

    fn remove(&mut self, volume: &str, bytes: u64) {
        self.adjust(volume, -i128::from(bytes));
    }

    fn peak(&self, volume: &str) -> u64 {
        self.peak.get(volume).copied().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SimulatedSpacePeaks {
    pub(super) install: u64,
    pub(super) vfs: u64,
    pub(super) work: u64,
}

fn payload_size(archive_entries: &BTreeMap<String, u64>, payload: &Path) -> u64 {
    archive_entries
        .get(&normalized_archive_path(payload))
        .copied()
        .unwrap_or(0)
}

fn logical_vfs_root(plan: &PatchPlan) -> PathBuf {
    plan.install_root.join(&plan.vfs_base_path)
}

fn physical_delete_path(plan: &PatchPlan, relative: &Path) -> PathBuf {
    if plan.vfs_destination != logical_vfs_root(plan) {
        if let Ok(vfs_relative) = relative.strip_prefix(&plan.vfs_base_path) {
            return plan.vfs_destination.join(vfs_relative);
        }
    }
    plan.install_root.join(relative)
}

fn logical_path_for_physical(plan: &PatchPlan, path: &Path) -> PathBuf {
    if let Ok(vfs_relative) = path.strip_prefix(&plan.vfs_destination) {
        return logical_vfs_root(plan).join(vfs_relative);
    }
    path.to_path_buf()
}

pub(super) fn simulate_space_peaks(
    plan: &PatchPlan,
    archive_entries: &BTreeMap<String, u64>,
    archive_uncompressed_bytes: u64,
    relocating_vfs_bytes: u64,
) -> Result<SimulatedSpacePeaks> {
    let install_key = storage_volume_group_key(&plan.install_root);
    let stage_key = storage_volume_group_key(&plan.stage_root);
    let vfs_key = storage_volume_group_key(&plan.vfs_destination);
    let work_key = plan.work_dir.as_deref().map(storage_volume_group_key);
    let mut ledger = VolumeSpaceLedger::default();

    // Extraction writes the full archive before commit starts.
    ledger.add(&stage_key, archive_uncompressed_bytes);

    // First-time external VFS setup copies each file before deleting its source.
    // A move on the same volume only renames the file and needs no more blocks.
    if relocating_vfs_bytes > 0 && install_key != vfs_key {
        ledger.add(&vfs_key, relocating_vfs_bytes);
        ledger.remove(&install_key, relocating_vfs_bytes);
    }

    let top_level = archive_entries
        .iter()
        .filter_map(|(name, size)| {
            let relative = PathBuf::from(name);
            (!is_patch_archive_control_path(&relative)).then_some((relative, *size))
        })
        .collect::<Vec<_>>();

    // Cross-volume commit workers may have destination temp copies in flight
    // while all sources still exist. Treat the full commit set as one wave;
    // this is conservative for any configured commit slot count.
    if stage_key != install_key {
        ledger.add(&install_key, top_level.iter().map(|(_, size)| *size).sum());
    }
    for (relative, size) in &top_level {
        if stage_key != install_key {
            ledger.remove(&stage_key, *size);
        }
        ledger.remove(
            &install_key,
            metadata_len(&plan.install_root.join(relative)),
        );
    }

    let outputs = plan
        .entries
        .iter()
        .map(|entry| plan.vfs_base_path.join(&entry.name))
        .collect::<BTreeSet<_>>();
    let bases = plan
        .entries
        .iter()
        .filter_map(|entry| match &entry.source {
            PlannedPatchSource::Hdiff { base, .. } => Some(base.clone()),
            PlannedPatchSource::AlreadyPresent | PlannedPatchSource::Local { .. } => None,
        })
        .collect::<BTreeSet<_>>();

    // The runner removes delete-only paths before patching, making that space
    // available to later waves. Signed deltas preserve this reclaimed capacity.
    for relative in &plan.delete_paths {
        let physical = physical_delete_path(plan, relative);
        if !bases.contains(&physical) && !outputs.contains(relative) {
            let existing = metadata_len(&plan.install_root.join(relative));
            ledger.remove(&storage_volume_group_key(&physical), existing);
        }
    }

    let mut current_sizes = BTreeMap::<PathBuf, u64>::new();
    for entry in &plan.entries {
        current_sizes
            .entry(entry.destination.clone())
            .or_insert_with(|| metadata_len(&logical_path_for_physical(plan, &entry.destination)));
        if let PlannedPatchSource::Hdiff {
            base, base_size, ..
        } = &entry.source
        {
            current_sizes.entry(base.clone()).or_insert_with(|| {
                let actual = metadata_len(&logical_path_for_physical(plan, base));
                if actual == 0 {
                    *base_size
                } else {
                    actual
                }
            });
        }
    }
    let delete_set = plan.delete_paths.iter().cloned().collect::<BTreeSet<_>>();
    let mut remaining_consumers = BTreeMap::<PathBuf, usize>::new();
    for entry in &plan.entries {
        if let PlannedPatchSource::Hdiff { base, .. } = &entry.source {
            *remaining_consumers.entry(base.clone()).or_default() += 1;
        }
    }

    for wave in entry_wave_indices(plan)? {
        reserve_wave_outputs(
            plan,
            archive_entries,
            &wave,
            &stage_key,
            &vfs_key,
            work_key.as_deref(),
            &mut ledger,
        );
        commit_wave_outputs(
            plan,
            archive_entries,
            wave,
            &stage_key,
            &vfs_key,
            &outputs,
            &delete_set,
            &mut current_sizes,
            &mut remaining_consumers,
            &mut ledger,
        );
    }

    Ok(SimulatedSpacePeaks {
        install: ledger.peak(&install_key),
        vfs: ledger.peak(&vfs_key),
        work: work_key.as_deref().map(|key| ledger.peak(key)).unwrap_or(0),
    })
}

fn reserve_wave_outputs(
    plan: &PatchPlan,
    archive_entries: &BTreeMap<String, u64>,
    wave: &[usize],
    stage_key: &str,
    vfs_key: &str,
    work_key: Option<&str>,
    ledger: &mut VolumeSpaceLedger,
) {
    // Every entry in a dependency wave can run concurrently. Account for all
    // output temps before applying any replacement/freeing effects.
    for index in wave {
        let entry = &plan.entries[*index];
        match &entry.source {
            PlannedPatchSource::AlreadyPresent => {}
            PlannedPatchSource::Local { payload } => {
                let staged = payload_size(archive_entries, payload);
                if stage_key != vfs_key {
                    ledger.add(vfs_key, entry.expected_size);
                } else if entry.expected_size > staged {
                    ledger.add(vfs_key, entry.expected_size - staged);
                }
            }
            PlannedPatchSource::Hdiff { .. } => {
                ledger.add(work_key.unwrap_or(vfs_key), entry.expected_size);
            }
        }
    }

    // External work output is copied to a destination-local temp while the
    // HDiff temp still exists, so both allocations overlap.
    if let Some(work_key) = work_key {
        for index in wave {
            let entry = &plan.entries[*index];
            if matches!(&entry.source, PlannedPatchSource::Hdiff { .. }) {
                ledger.add(vfs_key, entry.expected_size);
            }
        }
        for index in wave {
            let entry = &plan.entries[*index];
            if matches!(&entry.source, PlannedPatchSource::Hdiff { .. }) {
                ledger.remove(work_key, entry.expected_size);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_wave_outputs(
    plan: &PatchPlan,
    archive_entries: &BTreeMap<String, u64>,
    wave: Vec<usize>,
    stage_key: &str,
    vfs_key: &str,
    outputs: &BTreeSet<PathBuf>,
    delete_set: &BTreeSet<PathBuf>,
    current_sizes: &mut BTreeMap<PathBuf, u64>,
    remaining_consumers: &mut BTreeMap<PathBuf, usize>,
    ledger: &mut VolumeSpaceLedger,
) {
    for index in wave {
        let entry = &plan.entries[index];
        let existing = current_sizes
            .get(&entry.destination)
            .copied()
            .unwrap_or_else(|| metadata_len(&logical_path_for_physical(plan, &entry.destination)));
        match &entry.source {
            PlannedPatchSource::AlreadyPresent => {}
            PlannedPatchSource::Local { payload } => {
                let staged = payload_size(archive_entries, payload);
                if stage_key != vfs_key {
                    ledger.remove(stage_key, staged);
                } else if staged > entry.expected_size {
                    ledger.remove(stage_key, staged - entry.expected_size);
                }
                ledger.remove(vfs_key, existing);
                current_sizes.insert(entry.destination.clone(), entry.expected_size);
            }
            PlannedPatchSource::Hdiff { base, payload, .. } => {
                ledger.remove(stage_key, payload_size(archive_entries, payload));
                ledger.remove(vfs_key, existing);
                current_sizes.insert(entry.destination.clone(), entry.expected_size);
                release_last_deleted_base(
                    plan,
                    base,
                    outputs,
                    delete_set,
                    current_sizes,
                    remaining_consumers,
                    ledger,
                );
            }
        }
    }
}

fn release_last_deleted_base(
    plan: &PatchPlan,
    base: &Path,
    outputs: &BTreeSet<PathBuf>,
    delete_set: &BTreeSet<PathBuf>,
    current_sizes: &mut BTreeMap<PathBuf, u64>,
    remaining_consumers: &mut BTreeMap<PathBuf, usize>,
    ledger: &mut VolumeSpaceLedger,
) {
    let Some(remaining) = remaining_consumers.get_mut(base) else {
        return;
    };
    *remaining = remaining.saturating_sub(1);
    if *remaining != 0 {
        return;
    }
    let relative = if let Ok(vfs_relative) = base.strip_prefix(&plan.vfs_destination) {
        Some(plan.vfs_base_path.join(vfs_relative))
    } else {
        base.strip_prefix(&plan.install_root)
            .ok()
            .map(Path::to_path_buf)
    };
    if !relative
        .as_ref()
        .is_some_and(|relative| delete_set.contains(relative) && !outputs.contains(relative))
    {
        return;
    }
    let base_size = current_sizes.get(base).copied().unwrap_or(0);
    ledger.remove(&storage_volume_group_key(base), base_size);
    current_sizes.insert(base.to_path_buf(), 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::PlannedPatchEntry;

    fn hdiff_entry(
        root: &Path,
        name: &str,
        base: &str,
        payload: &str,
        size: u64,
    ) -> PlannedPatchEntry {
        PlannedPatchEntry {
            name: name.to_string(),
            destination: root.join("VFS").join(name),
            expected_md5: "output".to_string(),
            expected_size: size,
            source: PlannedPatchSource::Hdiff {
                base: root.join("VFS").join(base),
                payload: PathBuf::from(payload),
                base_md5: "base".to_string(),
                base_size: 1,
            },
        }
    }

    fn plan(root: &Path, entries: Vec<PlannedPatchEntry>) -> PatchPlan {
        PatchPlan {
            schema_version: PatchPlan::SCHEMA_VERSION,
            install_root: root.to_path_buf(),
            stage_root: root.join("stage"),
            vfs_base_path: PathBuf::from("VFS"),
            vfs_destination: root.join("VFS"),
            work_dir: None,
            entries,
            delete_paths: Vec::new(),
            deferred_paths: Vec::new(),
        }
    }

    #[test]
    fn dependency_wave_peak_sums_parallel_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("install");
        std::fs::create_dir_all(root.join("VFS")).unwrap();
        std::fs::write(root.join("VFS/base-a"), b"a").unwrap();
        std::fs::write(root.join("VFS/base-b"), b"b").unwrap();
        let entries = vec![
            hdiff_entry(&root, "a", "base-a", "patch/a", 40),
            hdiff_entry(&root, "b", "base-b", "patch/b", 60),
        ];
        let plan = plan(&root, entries);
        let archive = BTreeMap::from([("patch/a".to_string(), 5), ("patch/b".to_string(), 5)]);

        let peaks = simulate_space_peaks(&plan, &archive, 10, 0).unwrap();

        assert_eq!(peaks.install, 110);
    }

    #[test]
    fn delete_only_space_is_reused_by_later_wave() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("install");
        std::fs::create_dir_all(root.join("VFS")).unwrap();
        std::fs::write(root.join("VFS/base"), b"a").unwrap();
        std::fs::write(root.join("obsolete.bin"), vec![0u8; 80]).unwrap();
        let mut plan = plan(
            &root,
            vec![hdiff_entry(&root, "output", "base", "patch/output", 50)],
        );
        plan.delete_paths.push(PathBuf::from("obsolete.bin"));
        let archive = BTreeMap::from([("patch/output".to_string(), 10)]);

        let peaks = simulate_space_peaks(&plan, &archive, 100, 0).unwrap();

        assert_eq!(peaks.install, 100);
    }
}

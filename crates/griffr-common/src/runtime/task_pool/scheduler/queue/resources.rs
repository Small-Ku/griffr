use std::collections::{HashMap, HashSet};

use super::super::routing::{ResourceRequest, RunClass};
use crate::runtime::task_pool::{TaskPoolConfig, VolumeStreamingMode};

#[derive(Debug, Default)]
pub(crate) struct AdmissionSnapshot {
    pub(crate) reserved_write_volumes: HashSet<String>,
    pub(crate) storage_available_bytes: HashMap<String, Option<u64>>,
    pub(crate) queued_reuse_commits: usize,
}

#[derive(Debug, Default)]
pub(crate) struct ResourceState {
    pub(crate) network_in_use: usize,
    pub(crate) cpu_in_use: usize,
    pub(crate) blocking_in_use: usize,
    pub(crate) extract_in_use: usize,
    pub(crate) volume_reads: HashMap<String, usize>,
    pub(crate) volume_writes: HashMap<String, usize>,
    pub(crate) volume_metadata: HashMap<String, usize>,
    pub(crate) archive_savers: HashMap<String, usize>,
    pub(crate) archive_commits: HashMap<String, usize>,
    pub(crate) storage_reserved_bytes: HashMap<String, u64>,
    pub(crate) mutation_paths: HashSet<String>,
    pub(crate) reuse_commits_in_use: usize,
}

impl ResourceState {
    pub(crate) fn can_acquire(
        &self,
        request: &ResourceRequest,
        config: &TaskPoolConfig,
        admission: &AdmissionSnapshot,
    ) -> bool {
        match request.run {
            RunClass::AsyncIo => {}
            RunClass::Cpu if self.cpu_in_use >= config.cpu_slots => return false,
            RunClass::Blocking if self.blocking_in_use >= config.blocking_slots => return false,
            RunClass::Cpu | RunClass::Blocking => {}
        }
        if request.network.is_some() && self.network_in_use >= config.network_slots {
            return false;
        }
        if request.extract && self.extract_in_use >= config.extract_slots {
            return false;
        }
        if request.reuse_probe
            && self
                .reuse_commits_in_use
                .saturating_add(admission.queued_reuse_commits)
                >= config.reuse_queue_limit.max(1)
        {
            return false;
        }
        if request
            .archive_save_volumes
            .iter()
            .any(|volume| self.archive_savers.get(volume).copied().unwrap_or(0) > 0)
        {
            return false;
        }
        if request
            .archive_commit_volumes
            .iter()
            .any(|(volume, cross_volume)| {
                let limit = if *cross_volume { 3 } else { 1 };
                self.archive_commits.get(volume).copied().unwrap_or(0) >= limit
            })
        {
            return false;
        }
        if self.has_mutation_conflict(&request.mutation_paths) {
            return false;
        }
        if !self.can_reserve_storage(request, admission) {
            return false;
        }

        for volume in &request.read_volumes {
            if !self.can_acquire_volume(request, config, admission, volume) {
                return false;
            }
        }
        for volume in &request.write_volumes {
            if request.read_volumes.contains(volume) {
                continue;
            }
            if !self.can_acquire_volume(request, config, admission, volume) {
                return false;
            }
        }
        for volume in &request.metadata_volumes {
            if request.read_volumes.contains(volume) || request.write_volumes.contains(volume) {
                continue;
            }
            if !self.can_acquire_volume(request, config, admission, volume) {
                return false;
            }
        }
        true
    }

    fn can_acquire_volume(
        &self,
        request: &ResourceRequest,
        config: &TaskPoolConfig,
        admission: &AdmissionSnapshot,
        volume: &str,
    ) -> bool {
        let policy = config.volume_policy(volume);
        let wants_read = request
            .read_volumes
            .iter()
            .any(|item| item.as_str() == volume);
        let wants_write = request
            .write_volumes
            .iter()
            .any(|item| item.as_str() == volume);
        let wants_metadata = request
            .metadata_volumes
            .iter()
            .any(|item| item.as_str() == volume);
        let reads = self.volume_reads.get(volume).copied().unwrap_or(0);
        let writes = self.volume_writes.get(volume).copied().unwrap_or(0);
        let metadata = self.volume_metadata.get(volume).copied().unwrap_or(0);

        if reads.saturating_add(usize::from(wants_read)) > policy.read_limit
            || writes.saturating_add(usize::from(wants_write)) > policy.write_limit
            || metadata.saturating_add(usize::from(wants_metadata)) > policy.metadata_limit
        {
            return false;
        }

        let (current_pressure, requested_pressure) = match policy.streaming_mode {
            VolumeStreamingMode::Exclusive => {
                if (wants_read && writes > 0)
                    || (wants_write && reads > 0)
                    || (wants_metadata && (reads > 0 || writes > 0))
                    || ((wants_read || wants_write) && metadata > 0)
                {
                    return false;
                }
                if admission.reserved_write_volumes.contains(volume)
                    && !wants_write
                    && wants_metadata
                {
                    return false;
                }
                (reads.max(writes), usize::from(wants_read || wants_write))
            }
            VolumeStreamingMode::Mixed => (
                reads.saturating_add(writes),
                usize::from(wants_read).saturating_add(usize::from(wants_write)),
            ),
        };

        let reserve_for_waiting_writer = usize::from(
            admission.reserved_write_volumes.contains(volume)
                && requested_pressure > 0
                && !wants_write
                && writes < policy.write_limit,
        );
        current_pressure
            .saturating_add(requested_pressure)
            .saturating_add(reserve_for_waiting_writer)
            <= policy.streaming_pressure_limit
    }

    fn can_reserve_storage(
        &self,
        request: &ResourceRequest,
        admission: &AdmissionSnapshot,
    ) -> bool {
        request.storage_reservations.iter().all(|reservation| {
            let active = self
                .storage_reserved_bytes
                .get(&reservation.volume)
                .copied()
                .unwrap_or(0);
            if active == 0 {
                return true;
            }
            let Some(available) = admission
                .storage_available_bytes
                .get(&reservation.volume)
                .copied()
                .flatten()
            else {
                return true;
            };
            storage_reservation_fits(active, reservation.bytes, available)
        })
    }

    pub(crate) fn has_mutation_conflict(&self, requested: &[String]) -> bool {
        requested.iter().any(|requested| {
            self.mutation_paths
                .iter()
                .any(|active| mutation_paths_conflict(requested, active))
        })
    }

    pub(crate) fn acquire(&mut self, request: &ResourceRequest) {
        match request.run {
            RunClass::AsyncIo => {}
            RunClass::Cpu => self.cpu_in_use = self.cpu_in_use.saturating_add(1),
            RunClass::Blocking => self.blocking_in_use = self.blocking_in_use.saturating_add(1),
        }
        if request.network.is_some() {
            self.network_in_use = self.network_in_use.saturating_add(1);
        }
        if request.extract {
            self.extract_in_use = self.extract_in_use.saturating_add(1);
        }
        for volume in &request.read_volumes {
            *self.volume_reads.entry(volume.clone()).or_default() += 1;
        }
        for volume in &request.write_volumes {
            *self.volume_writes.entry(volume.clone()).or_default() += 1;
        }
        for volume in &request.metadata_volumes {
            *self.volume_metadata.entry(volume.clone()).or_default() += 1;
        }
        for volume in &request.archive_save_volumes {
            *self.archive_savers.entry(volume.clone()).or_default() += 1;
        }
        for (volume, _) in &request.archive_commit_volumes {
            *self.archive_commits.entry(volume.clone()).or_default() += 1;
        }
        for reservation in &request.storage_reservations {
            let reserved = self
                .storage_reserved_bytes
                .entry(reservation.volume.clone())
                .or_default();
            *reserved = reserved.saturating_add(reservation.bytes);
        }
        self.mutation_paths
            .extend(request.mutation_paths.iter().cloned());
        if request.reuse_commit {
            self.reuse_commits_in_use = self.reuse_commits_in_use.saturating_add(1);
        }
    }

    pub(crate) fn release(&mut self, request: &ResourceRequest) {
        match request.run {
            RunClass::AsyncIo => {}
            RunClass::Cpu => self.cpu_in_use = self.cpu_in_use.saturating_sub(1),
            RunClass::Blocking => self.blocking_in_use = self.blocking_in_use.saturating_sub(1),
        }
        if request.network.is_some() {
            self.network_in_use = self.network_in_use.saturating_sub(1);
        }
        if request.extract {
            self.extract_in_use = self.extract_in_use.saturating_sub(1);
        }
        for volume in &request.read_volumes {
            decrement(&mut self.volume_reads, volume);
        }
        for volume in &request.write_volumes {
            decrement(&mut self.volume_writes, volume);
        }
        for volume in &request.metadata_volumes {
            decrement(&mut self.volume_metadata, volume);
        }
        for volume in &request.archive_save_volumes {
            decrement(&mut self.archive_savers, volume);
        }
        for (volume, _) in &request.archive_commit_volumes {
            decrement(&mut self.archive_commits, volume);
        }
        for reservation in &request.storage_reservations {
            decrement_bytes(
                &mut self.storage_reserved_bytes,
                &reservation.volume,
                reservation.bytes,
            );
        }
        for path in &request.mutation_paths {
            self.mutation_paths.remove(path);
        }
        if request.reuse_commit {
            self.reuse_commits_in_use = self.reuse_commits_in_use.saturating_sub(1);
        }
    }
}

fn storage_reservation_fits(active: u64, requested: u64, available: u64) -> bool {
    // A single writer is never rejected by an install-wide estimate. Its own
    // preallocation remains the authoritative space check. Once a writer is
    // active, later writers must fit beside its declared peak allocation.
    active == 0 || active.saturating_add(requested) <= available
}

fn mutation_paths_conflict(left: &str, right: &str) -> bool {
    left == right || mutation_path_contains(left, right) || mutation_path_contains(right, left)
}

fn mutation_path_contains(parent: &str, child: &str) -> bool {
    child.strip_prefix(parent).is_some_and(|suffix| {
        suffix.starts_with('/') || (parent.ends_with('/') && !suffix.is_empty())
    })
}

fn decrement(counts: &mut HashMap<String, usize>, key: &str) {
    let should_remove = if let Some(count) = counts.get_mut(key) {
        *count = count.saturating_sub(1);
        *count == 0
    } else {
        false
    };
    if should_remove {
        counts.remove(key);
    }
}

fn decrement_bytes(counts: &mut HashMap<String, u64>, key: &str, bytes: u64) {
    let should_remove = if let Some(count) = counts.get_mut(key) {
        *count = count.saturating_sub(bytes);
        *count == 0
    } else {
        false
    };
    if should_remove {
        counts.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        mutation_paths_conflict, storage_reservation_fits, AdmissionSnapshot, ResourceState,
    };
    use crate::runtime::task_pool::scheduler::routing::{ResourceRequest, StorageReservation};
    use crate::runtime::task_pool::TaskPoolConfig;
    use std::path::PathBuf;

    #[test]
    fn first_storage_writer_is_not_rejected_by_a_global_estimate() {
        assert!(storage_reservation_fits(0, u64::MAX, 1));
    }

    #[test]
    fn concurrent_storage_reservations_must_fit_current_free_space() {
        assert!(storage_reservation_fits(10, 20, 30));
        assert!(!storage_reservation_fits(10, 21, 30));
    }

    #[test]
    fn active_writer_uses_the_shared_volume_space_snapshot() {
        let active = ResourceRequest {
            storage_reservations: vec![StorageReservation {
                volume: "volume-a".to_string(),
                probe_path: PathBuf::from("volume-a"),
                bytes: 10,
            }],
            ..ResourceRequest::default()
        };
        let fitting = ResourceRequest {
            storage_reservations: vec![StorageReservation {
                volume: "volume-a".to_string(),
                probe_path: PathBuf::from("volume-a"),
                bytes: 20,
            }],
            ..ResourceRequest::default()
        };
        let too_large = ResourceRequest {
            storage_reservations: vec![StorageReservation {
                volume: "volume-a".to_string(),
                probe_path: PathBuf::from("volume-a"),
                bytes: 21,
            }],
            ..ResourceRequest::default()
        };
        let admission = AdmissionSnapshot {
            storage_available_bytes: [("volume-a".to_string(), Some(30))].into_iter().collect(),
            ..AdmissionSnapshot::default()
        };
        let mut state = ResourceState::default();
        state.acquire(&active);

        assert!(state.can_acquire(&fitting, &TaskPoolConfig::default(), &admission));
        assert!(!state.can_acquire(&too_large, &TaskPoolConfig::default(), &admission));
    }

    #[test]
    fn storage_reservations_are_released_with_task_resources() {
        let request = ResourceRequest {
            storage_reservations: vec![StorageReservation {
                volume: "volume-a".to_string(),
                probe_path: PathBuf::from("volume-a"),
                bytes: 32,
            }],
            ..ResourceRequest::default()
        };
        let mut state = ResourceState::default();

        state.acquire(&request);
        assert_eq!(state.storage_reserved_bytes.get("volume-a"), Some(&32));
        state.release(&request);
        assert!(!state.storage_reserved_bytes.contains_key("volume-a"));
    }

    #[test]
    fn mutation_paths_reject_ancestor_conflicts() {
        assert!(mutation_paths_conflict(
            "c:/games",
            "c:/games/endfield/file.bin"
        ));
        assert!(mutation_paths_conflict("/", "/tmp/file.bin"));
        assert!(mutation_paths_conflict("c:/", "c:/games/file.bin"));
    }

    #[test]
    fn mutation_paths_allow_sibling_files() {
        assert!(!mutation_paths_conflict(
            "c:/games/endfield/a.bin",
            "c:/games/endfield/b.bin"
        ));
        assert!(!mutation_paths_conflict("c:/games", "c:/games-old"));
    }
}

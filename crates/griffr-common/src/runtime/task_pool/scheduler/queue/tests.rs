use super::{AdmissionSnapshot, ResourceState, SchedulerQueue};
use crate::runtime::task_pool::scheduler::routing::{ExecutionClass, ResourceRequest};
use crate::runtime::task_pool::scheduler::TaskPriority;
use crate::runtime::task_pool::{
    NodeId, Task, TaskPoolConfig, VolumeIoPolicy, VolumeStreamingMode,
};
use std::path::PathBuf;

fn hardlink(name: &str) -> Task {
    Task::Hardlink {
        src: PathBuf::from(format!("{name}.src")),
        dest: PathBuf::from(name),
    }
}

fn resources(volume: &str) -> ResourceRequest {
    ResourceRequest {
        execution: ExecutionClass::Blocking,
        write_volumes: vec![volume.to_string()],
        ..ResourceRequest::default()
    }
}

fn read(volume: &str) -> ResourceRequest {
    ResourceRequest {
        execution: ExecutionClass::AsyncIo,
        read_volumes: vec![volume.to_string()],
        ..ResourceRequest::default()
    }
}

fn write(volume: &str) -> ResourceRequest {
    ResourceRequest {
        execution: ExecutionClass::Blocking,
        write_volumes: vec![volume.to_string()],
        ..ResourceRequest::default()
    }
}

fn metadata(volume: &str) -> ResourceRequest {
    ResourceRequest {
        execution: ExecutionClass::Blocking,
        metadata_volumes: vec![volume.to_string()],
        ..ResourceRequest::default()
    }
}

#[test]
fn unavailable_blocking_pool_does_not_stall_async_admission() {
    let mut queue = SchedulerQueue::default();
    let config = TaskPoolConfig::default();
    queue.push(
        NodeId::from_index(0),
        hardlink("blocking"),
        ResourceRequest {
            execution: ExecutionClass::Blocking,
            ..ResourceRequest::default()
        },
        TaskPriority::Bulk,
    );
    queue.push(
        NodeId::from_index(1),
        hardlink("async"),
        ResourceRequest {
            execution: ExecutionClass::AsyncIo,
            ..ResourceRequest::default()
        },
        TaskPriority::Bulk,
    );

    let selected = queue.pop_next(&config, false).unwrap();
    assert!(matches!(
        selected.task,
        Task::Hardlink { ref dest, .. } if dest == &PathBuf::from("async")
    ));
    queue.release(&selected.resources);
    assert!(queue.pop_next(&config, false).is_none());

    let selected = queue.pop_next(&config, true).unwrap();
    assert!(matches!(
        selected.task,
        Task::Hardlink { ref dest, .. } if dest == &PathBuf::from("blocking")
    ));
    queue.release(&selected.resources);
}

#[test]
fn volume_writer_blocks_only_the_same_volume() {
    let mut queue = SchedulerQueue::default();
    let config = TaskPoolConfig::default();
    queue.push(
        NodeId::from_index(0),
        hardlink("a"),
        resources("volume-a"),
        TaskPriority::Bulk,
    );
    queue.push(
        NodeId::from_index(1),
        hardlink("b"),
        resources("volume-b"),
        TaskPriority::Bulk,
    );

    let first = queue.pop_next(&config, true).unwrap();
    let second = queue.pop_next(&config, true).unwrap();
    queue.release(&first.resources);
    queue.release(&second.resources);
}

#[test]
fn default_policy_uses_nvme_limits_and_allows_mixed_io() {
    let config = TaskPoolConfig {
        blocking_slots: 200,
        ..TaskPoolConfig::default()
    };
    let admission = AdmissionSnapshot::default();
    let reader = read("volume-a");
    let writer = write("volume-a");
    let metadata = metadata("volume-a");
    let mut state = ResourceState::default();

    assert_eq!(
        config.default_volume_policy,
        VolumeIoPolicy::new(16, 16, 128, 32, VolumeStreamingMode::Mixed)
    );
    for _ in 0..128 {
        state.acquire(&metadata);
    }
    assert!(!state.can_acquire(&metadata, &config, &admission));

    for _ in 0..16 {
        assert!(state.can_acquire(&reader, &config, &admission));
        state.acquire(&reader);
    }
    for _ in 0..16 {
        assert!(state.can_acquire(&writer, &config, &admission));
        state.acquire(&writer);
    }
    assert!(!state.can_acquire(&reader, &config, &admission));
    assert!(!state.can_acquire(&writer, &config, &admission));
}

#[test]
fn same_volume_copy_consumes_both_mixed_streaming_credits() {
    let config = TaskPoolConfig {
        default_volume_policy: VolumeIoPolicy::new(2, 1, 1, 3, VolumeStreamingMode::Mixed),
        ..Default::default()
    };
    let admission = AdmissionSnapshot::default();
    let copy = ResourceRequest {
        read_volumes: vec!["volume-a".to_string()],
        write_volumes: vec!["volume-a".to_string()],
        ..ResourceRequest::default()
    };
    let mut state = ResourceState::default();

    assert!(state.can_acquire(&copy, &config, &admission));
    state.acquire(&copy);
    assert!(state.can_acquire(&read("volume-a"), &config, &admission));
    assert!(!state.can_acquire(&write("volume-a"), &config, &admission));
}

#[test]
fn exclusive_policy_keeps_streaming_and_metadata_separate() {
    let config = TaskPoolConfig {
        default_volume_policy: VolumeIoPolicy::new(1, 1, 1, 1, VolumeStreamingMode::Exclusive),
        ..Default::default()
    };
    let admission = AdmissionSnapshot::default();
    let reader = read("volume-a");
    let mut state = ResourceState::default();

    state.acquire(&reader);
    assert!(!state.can_acquire(&write("volume-a"), &config, &admission));
    assert!(!state.can_acquire(&metadata("volume-a"), &config, &admission));

    let copy = ResourceRequest {
        read_volumes: vec!["volume-a".to_string()],
        write_volumes: vec!["volume-a".to_string()],
        ..ResourceRequest::default()
    };
    assert!(ResourceState::default().can_acquire(&copy, &config, &admission));
}

#[test]
fn aged_writer_reserves_pressure_without_stopping_all_mixed_mode_readers() {
    let config = TaskPoolConfig::default();
    let admission = AdmissionSnapshot {
        reserved_write_volumes: ["volume-a".to_string()].into_iter().collect(),
        ..AdmissionSnapshot::default()
    };
    let reader = read("volume-a");
    let writer = write("volume-a");
    let mut state = ResourceState::default();

    assert!(state.can_acquire(&reader, &config, &admission));
    state.acquire(&reader);
    assert!(state.can_acquire(&reader, &config, &admission));
    state.acquire(&reader);
    assert!(state.can_acquire(&writer, &config, &admission));
    assert!(state.can_acquire(&metadata("volume-a"), &config, &admission));
}

#[test]
fn aged_writer_stops_new_exclusive_work_until_the_volume_drains() {
    let config = TaskPoolConfig {
        default_volume_policy: VolumeIoPolicy::new(1, 1, 1, 1, VolumeStreamingMode::Exclusive),
        ..Default::default()
    };
    let admission = AdmissionSnapshot {
        reserved_write_volumes: ["volume-a".to_string()].into_iter().collect(),
        ..AdmissionSnapshot::default()
    };

    assert!(!ResourceState::default().can_acquire(&read("volume-a"), &config, &admission,));
    assert!(!ResourceState::default().can_acquire(&metadata("volume-a"), &config, &admission,));
    assert!(ResourceState::default().can_acquire(&write("volume-a"), &config, &admission,));
}

#[test]
fn per_volume_override_can_select_an_exclusive_policy() {
    let mut config = TaskPoolConfig::default();
    config.volume_policies.insert(
        "volume-a".to_string(),
        VolumeIoPolicy::new(1, 1, 1, 1, VolumeStreamingMode::Exclusive),
    );
    let admission = AdmissionSnapshot::default();
    let mut state = ResourceState::default();

    state.acquire(&read("volume-a"));
    assert!(!state.can_acquire(&write("volume-a"), &config, &admission));
    assert!(state.can_acquire(&write("volume-b"), &config, &admission));
}

#[test]
fn reuse_pipeline_window_limits_verified_but_uncommitted_files() {
    let config = TaskPoolConfig {
        reuse_pipeline_window: 1,
        ..Default::default()
    };
    let probe = ResourceRequest {
        reuse_probe: true,
        read_volumes: vec!["volume-a".to_string()],
        ..ResourceRequest::default()
    };
    let admission = AdmissionSnapshot {
        queued_reuse_commits: 1,
        ..AdmissionSnapshot::default()
    };
    assert!(!ResourceState::default().can_acquire(&probe, &config, &admission));
}

#[test]
fn runnable_tasks_prefer_smaller_work_on_the_same_backlogged_volume() {
    let mut queue = SchedulerQueue::default();
    let config = TaskPoolConfig::default();
    let mut large = resources("volume-a");
    large.execution = ExecutionClass::Blocking;
    large.estimated_bytes = 1024;
    let mut small = resources("volume-a");
    small.execution = ExecutionClass::Blocking;
    small.estimated_bytes = 1;
    queue.push(
        NodeId::from_index(0),
        hardlink("large"),
        large,
        TaskPriority::Bulk,
    );
    queue.push(
        NodeId::from_index(1),
        hardlink("small"),
        small,
        TaskPriority::Bulk,
    );

    let selected = queue.pop_next(&config, true).unwrap();
    assert!(matches!(
        selected.task,
        Task::Hardlink { ref dest, .. } if dest == &PathBuf::from("small")
    ));
    queue.release(&selected.resources);
}

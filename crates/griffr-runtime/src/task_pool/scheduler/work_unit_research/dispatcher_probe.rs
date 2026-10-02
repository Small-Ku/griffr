//! Real, single-file read/hash pilot over the repository Dispatcher. This does
//! not measure the SIMD batch verifier, network, archive codec, or production coordinator.
use super::*;
use md5::{Digest, Md5};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

struct ReadState {
    file: File,
    hash: Md5,
    bytes: usize,
}
struct ReadFinish {
    state: ReadState,
    done: bool,
    run_ns: u128,
    resume_ns: u128,
}

fn read_quantum(mut state: ReadState, bound: usize) -> ReadFinish {
    let started = Instant::now();
    let resume_started = Instant::now();
    // Retain the open descriptor and incremental hash; no replay, seek, or reopen.
    // Allocation is deliberately charged to resume rather than useful hash work.
    let mut buffer = vec![0u8; bound.min(256 * 1024)];
    let resume_ns = resume_started.elapsed().as_nanos();
    let mut consumed = 0;
    let mut done = false;
    while consumed < bound {
        let limit = buffer.len().min(bound - consumed);
        let read = state.file.read(&mut buffer[..limit]).unwrap();
        if read == 0 {
            done = true;
            break;
        }
        state.hash.update(&buffer[..read]);
        state.bytes += read;
        consumed += read;
    }
    ReadFinish {
        state,
        done,
        run_ns: started.elapsed().as_nanos(),
        resume_ns,
    }
}

#[derive(Serialize)]
struct Probe {
    kind: &'static str,
    policy: Policy,
    repeat: usize,
    bytes: usize,
    wall_ns: u128,
    scheduler_ns: u128,
    resume_ns: u128,
    max_non_arbitration_ns: u128,
    max_worker_run_ns: u128,
    dispatches: usize,
    requeues: usize,
}
fn probe(
    path: &Path,
    expected: &[u8],
    policy: Policy,
    repeat: usize,
    dispatcher: &Arc<compio::dispatcher::Dispatcher>,
) -> Probe {
    let mut state = ReadState {
        file: File::open(path).unwrap(),
        hash: Md5::new(),
        bytes: 0,
    };
    let mut graph = TaskGraph::from_tasks(vec![task(0)]);
    let mut ready = graph.start().pop().unwrap();
    let mut resources = ResourceState::default();
    let demand = ResourceRequest {
        run: RunClass::Cpu,
        read_volumes: vec!["probe-disk".into()],
        ..ResourceRequest::default()
    };
    let config = TaskPoolConfig::default();
    let admission = AdmissionSnapshot::default();
    let mut bound = 256 * 1024;
    let mut dispatches = 0;
    let mut scheduler_ns = 0;
    let mut resume_ns = 0;
    let mut max_non_arbitration_ns = 0;
    let mut max_worker_run_ns = 0;
    let started = Instant::now();
    loop {
        let scheduling_started = Instant::now();
        assert!(resources.can_acquire(&demand, &config, &admission));
        resources.acquire(&demand);
        graph.mark_running(ready.id).unwrap();
        let bytes = match policy {
            Policy::Coarse => usize::MAX,
            Policy::FixedSmall => 256 * 1024,
            Policy::FixedLarge => 4 * 1024 * 1024,
            Policy::Progressive | Policy::SoftDeadline => bound,
        };
        // A soft deadline has no effect when this is the only ready node.
        let (tx, rx) = flume::bounded(1);
        let receiver = dispatcher
            .dispatch_blocking(move || {
                tx.send(read_quantum(state, bytes)).unwrap();
            })
            .expect("idle probe Dispatcher must admit one blocking task");
        drop(receiver);
        scheduler_ns += scheduling_started.elapsed().as_nanos();
        let finish = rx.recv().unwrap();
        dispatches += 1;
        resume_ns += finish.resume_ns;
        let finishing_started = Instant::now();
        max_worker_run_ns = max_worker_run_ns.max(finish.run_ns);
        state = finish.state;
        resources.release(&demand);
        max_non_arbitration_ns =
            max_non_arbitration_ns.max(scheduling_started.elapsed().as_nanos());
        let next = graph
            .finish(
                ready.id,
                if finish.done {
                    TaskRun::succeeded()
                } else {
                    TaskRun::then(ready.task)
                },
            )
            .unwrap();
        scheduler_ns += finishing_started.elapsed().as_nanos();
        if finish.done {
            break;
        }
        ready = next.into_iter().next().unwrap();
        bound = (bound * 2).min(4 * 1024 * 1024);
    }
    let wall_ns = started.elapsed().as_nanos();
    assert_eq!(state.hash.finalize().as_slice(), expected);
    assert_eq!(graph.node_count(), 1);
    assert_eq!(graph.summary().succeeded_nodes, 1);
    assert_eq!(resources.cpu_in_use, 0);
    Probe {
        kind: "dispatcher-pilot",
        policy,
        repeat,
        bytes: state.bytes,
        wall_ns,
        scheduler_ns,
        resume_ns,
        max_non_arbitration_ns,
        max_worker_run_ns,
        dispatches,
        requeues: dispatches - 1,
    }
}

#[test]
fn read_hash_resume_preserves_bytes_across_safe_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    for size in [0, 55, 64, 65, 256 * 1024 + 73] {
        let payload = vec![0x53; size];
        let path = directory.path().join("input");
        std::fs::write(&path, &payload).unwrap();
        let mut state = ReadState {
            file: File::open(path).unwrap(),
            hash: Md5::new(),
            bytes: 0,
        };
        loop {
            let finish = read_quantum(state, 61);
            state = finish.state;
            if finish.done {
                break;
            }
        }
        assert_eq!(state.bytes, size);
        assert_eq!(state.hash.finalize(), Md5::digest(&payload));
    }
}

#[derive(Serialize)]
struct CanonicalProbe {
    kind: &'static str,
    repeat: usize,
    files: usize,
    bytes: usize,
    wall_ns: u128,
    queue_p50_ns: u128,
    queue_p95_ns: u128,
    run_p50_ns: u128,
    run_p95_ns: u128,
    dispatch_finishes: usize,
}
fn canonical_probe(
    paths: &[std::path::PathBuf],
    expected: &str,
    repeat: usize,
    runner: &mut crate::task_pool::TaskPoolRunner,
) {
    let tasks = paths
        .iter()
        .map(|path| Task::Verify {
            path: path.clone(),
            logical_path: path.display().to_string(),
            expected_hash: expected.into(),
            expected_size: Some(16 * 1024 * 1024),
            on_fail: None,
        })
        .collect();
    let started = Instant::now();
    let result = runner
        .run_batch(tasks, crate::task_pool::TaskProgress::disabled())
        .unwrap();
    let wall_ns = started.elapsed().as_nanos();
    assert_eq!(result.metrics.graph.succeeded_nodes, paths.len());
    assert_eq!(result.metrics.graph.failed_nodes, 0);
    let output = CanonicalProbe {
        kind: "canonical-verify",
        repeat,
        files: paths.len(),
        bytes: paths.len() * 16 * 1024 * 1024,
        wall_ns,
        queue_p50_ns: result.metrics.queue_wait_p50.as_nanos(),
        queue_p95_ns: result.metrics.queue_wait_p95.as_nanos(),
        run_p50_ns: result.metrics.task_duration_p50.as_nanos(),
        run_p95_ns: result.metrics.task_duration_p95.as_nanos(),
        dispatch_finishes: result.metrics.finished_tasks,
    };
    if repeat > 0 {
        println!("{}", serde_json::to_string(&output).unwrap());
    }
}

#[test]
#[ignore = "Issue #1 warm-cache single-file Dispatcher timing probe"]
fn work_unit_dispatcher_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.bin");
    let payload: Vec<_> = (0..16 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &payload).unwrap();
    let expected = Md5::digest(&payload);
    let paths: Vec<_> = (0..4)
        .map(|index| {
            let path = directory.path().join(format!("canonical-{index}.bin"));
            std::fs::write(&path, &payload).unwrap();
            path
        })
        .collect();
    let expected_hex = griffr_core::to_hex(&expected);
    let config = TaskPoolConfig {
        dispatcher_threads: 2,
        cpu_slots: 1,
        blocking_slots: 1,
        ..TaskPoolConfig::default()
    };
    let dispatcher = crate::task_pool::scheduler::build_dispatcher(&config).unwrap();
    let mut runner = crate::task_pool::TaskPoolRunner::new(config).unwrap();
    canonical_probe(&paths[..1], &expected_hex, 0, &mut runner);
    canonical_probe(&paths, &expected_hex, 0, &mut runner);
    // One warm-up per candidate. Rotate ordering to reduce systematic drift.
    for policy in POLICIES {
        probe(&path, &expected, policy, 0, &dispatcher);
    }
    for repeat in 1..=7 {
        canonical_probe(&paths[..1], &expected_hex, repeat, &mut runner);
        canonical_probe(&paths, &expected_hex, repeat, &mut runner);
        for offset in 0..POLICIES.len() {
            let policy = POLICIES[(repeat + offset) % POLICIES.len()];
            println!(
                "{}",
                serde_json::to_string(&probe(&path, &expected, policy, repeat, &dispatcher))
                    .unwrap()
            );
        }
    }
}

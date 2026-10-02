//! Issue #1 pilot. No production policy or second runtime is exported.
//! Model time is in microseconds; real graph/admission code owns correctness.
use super::queue::{AdmissionSnapshot, ResourceState};
use super::routing::{NetworkClass, ResourceRequest, RunClass, StorageReservation};
use crate::task_pool::graph::{ReadyTask, TaskRun};
use crate::task_pool::{Task, TaskGraph, TaskPoolConfig, VolumeIoPolicy, VolumeStreamingMode};
use serde::Serialize;
use std::time::Instant;

const ATOM_US: u64 = 1_000;
const AGE_LIMIT_US: u64 = 20_000;
const DISPATCH_US: u64 = 20;
const SMALL: usize = 2;
const LARGE: usize = 32;
const CHANNELS: usize = 6; // network, CPU, blocking, read, write, extract

#[derive(Clone, Copy, Debug, Serialize)]
enum Policy {
    Coarse,
    FixedSmall,
    FixedLarge,
    Progressive,
    SoftDeadline,
}
const POLICIES: [Policy; 5] = [
    Policy::Coarse,
    Policy::FixedSmall,
    Policy::FixedLarge,
    Policy::Progressive,
    Policy::SoftDeadline,
];

#[derive(Clone, Copy)]
enum Phase {
    Download,
    Read,
    Extract,
    Write,
    Cpu,
}
impl Phase {
    fn channels(self) -> [bool; CHANNELS] {
        match self {
            Self::Download => [true, false, false, false, true, false],
            Self::Read => [false, false, true, true, false, false],
            Self::Extract => [false, true, false, true, true, true],
            Self::Write => [false, false, true, false, true, false],
            Self::Cpu => [false, true, false, false, false, false],
        }
    }
}
struct Work {
    atoms: Vec<Phase>,
    cursor: usize,
    next_bound: usize,
    cost_ewma: u64,
    arrival: u64,
    deadline: Option<u64>,
    foreground: bool,
    ready_at: u64,
    finished_at: Option<u64>,
    parent: Option<usize>,
}
fn task(index: usize) -> Task {
    Task::Verify {
        path: format!("research-{index}").into(),
        logical_path: format!("research-{index}"),
        expected_hash: "00".repeat(16).into(),
        expected_size: Some(1),
        on_fail: None,
    }
}
fn work(phases: &[(Phase, usize)], arrival: u64, foreground: bool, parent: Option<usize>) -> Work {
    Work {
        atoms: phases
            .iter()
            .flat_map(|(phase, count)| std::iter::repeat_n(*phase, *count))
            .collect(),
        cursor: 0,
        next_bound: SMALL,
        cost_ewma: ATOM_US * 2,
        arrival,
        deadline: foreground.then_some(arrival + 16_000),
        foreground,
        ready_at: arrival,
        finished_at: None,
        parent,
    }
}
fn workload(name: &str) -> Vec<Work> {
    match name {
        "mixed" | "costly-resume" => {
            let mut jobs: Vec<_> = (0..8)
                .map(|_| {
                    work(
                        &[
                            (Phase::Download, 8),
                            (Phase::Read, 8),
                            (Phase::Extract, 40),
                            (Phase::Write, 8),
                        ],
                        0,
                        false,
                        None,
                    )
                })
                .collect();
            jobs.extend((0..16).map(|i| work(&[(Phase::Cpu, 2)], 5_000 + i * 3_000, true, None)));
            jobs.push(work(&[(Phase::Write, 2)], 0, false, Some(0)));
            jobs
        }
        "solo" => vec![work(&[(Phase::Cpu, 64)], 0, false, None)],
        "deadline-pressure" => {
            let mut jobs: Vec<_> = (0..8)
                .map(|_| work(&[(Phase::Cpu, 64)], 0, false, None))
                .collect();
            jobs.extend((0..16).map(|i| work(&[(Phase::Cpu, 2)], 5_000 + i * 1_000, true, None)));
            jobs
        }
        "throughput" => (0..16)
            .map(|_| work(&[(Phase::Cpu, 64)], 0, false, None))
            .collect(),
        "tiny" => (0..32)
            .map(|_| work(&[(Phase::Cpu, 1)], 0, false, None))
            .collect(),
        _ => panic!("unknown workload"),
    }
}
fn quantum(job: &Work, policy: Policy, contention: bool) -> usize {
    let left = job.atoms.len() - job.cursor;
    match policy {
        Policy::Coarse => left,
        Policy::FixedSmall => SMALL.min(left),
        Policy::FixedLarge => LARGE.min(left),
        Policy::Progressive | Policy::SoftDeadline => {
            // Only proven safe atoms may be grouped. Pressure caps the next run.
            if contention { SMALL } else { job.next_bound }.min(left)
        }
    }
}
fn request(index: usize, job: &Work, count: usize) -> ResourceRequest {
    let mut channels = [false; CHANNELS];
    for phase in &job.atoms[job.cursor..job.cursor + count] {
        for (used, wants) in channels.iter_mut().zip(phase.channels()) {
            *used |= wants;
        }
    }
    ResourceRequest {
        run: if channels[1] {
            RunClass::Cpu
        } else if channels[2] {
            RunClass::Blocking
        } else {
            RunClass::AsyncIo
        },
        network: channels[0].then_some(NetworkClass::General),
        read_volumes: if channels[3] {
            vec!["disk".into()]
        } else {
            vec![]
        },
        write_volumes: if channels[4] {
            vec!["disk".into()]
        } else {
            vec![]
        },
        extract: channels[5],
        mutation_paths: if channels[4] {
            vec![format!("/output/{index}")]
        } else {
            vec![]
        },
        storage_reservations: if channels[4] {
            vec![StorageReservation {
                volume: "disk".into(),
                probe_path: "/unused-model-path".into(),
                bytes: 8,
            }]
        } else {
            vec![]
        },
        estimated_bytes: count as u64,
        ..ResourceRequest::default()
    }
}
fn held_channels(request: &ResourceRequest) -> [bool; CHANNELS] {
    [
        request.network.is_some(),
        request.run == RunClass::Cpu,
        request.run == RunClass::Blocking,
        !request.read_volumes.is_empty(),
        !request.write_volumes.is_empty(),
        request.extract,
    ]
}
struct Active {
    ready: ReadyTask,
    resources: ResourceRequest,
    count: usize,
    started: u64,
    end: u64,
}

#[derive(Debug, Serialize)]
struct Evidence {
    workload: String,
    policy: Policy,
    model_us: u64,
    p50_us: u64,
    p95_us: u64,
    foreground_p95_us: u64,
    background_p95_us: u64,
    max_non_arbitration_us: u64,
    max_ready_wait_us: u64,
    scheduler_model_us: u64,
    resume_model_us: u64,
    dispatches: usize,
    requeues: usize,
    deadline_misses: usize,
    critical_unlock_us: u64,
    useful_utilization: [f64; CHANNELS],
    held_utilization: [f64; CHANNELS],
    max_idle_gap_us: [u64; CHANNELS],
    throughput_atoms_per_s: f64,
    capacity_violations: usize,
    scheduler_host_ns: u128,
}
fn percentile(values: &mut [u64], percent: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[(values.len() * percent).div_ceil(100).saturating_sub(1)]
}
fn simulate(name: &str, policy: Policy, resume_us: u64) -> Evidence {
    let mut jobs = workload(name);
    let mut builder = TaskGraph::builder();
    let mut ids = Vec::new();
    for (i, job) in jobs.iter().enumerate() {
        let deps = job.parent.map(|parent| ids[parent]);
        ids.push(builder.add_task(task(i), deps).unwrap());
    }
    let mut graph = builder.build();
    let mut ready = graph.start();
    let config = TaskPoolConfig {
        cpu_slots: 1,
        blocking_slots: 1,
        network_slots: 1,
        extract_slots: 1,
        default_volume_policy: VolumeIoPolicy::new(1, 1, 1, 2, VolumeStreamingMode::Mixed),
        ..TaskPoolConfig::default()
    };
    let admission = AdmissionSnapshot {
        storage_available_bytes: [("disk".into(), Some(16))].into_iter().collect(),
        ..AdmissionSnapshot::default()
    };
    let mut resources = ResourceState::default();
    let mut active: Vec<Active> = Vec::new();
    let mut now = 0;
    let mut streak = 0usize;
    let mut dispatches = 0;
    let mut requeues = 0;
    let mut max_wait = 0;
    let mut max_run = 0;
    let mut resume_total = 0;
    let mut host_ns = 0;
    let mut useful = [0u64; CHANNELS];
    let mut held = [0u64; CHANNELS];
    let mut intervals: [Vec<(u64, u64)>; CHANNELS] = std::array::from_fn(|_| Vec::new());
    while graph.has_unresolved() {
        let selection_started = Instant::now();
        loop {
            let eligible = ready
                .iter()
                .filter(|r| jobs[r.id.index()].arrival <= now)
                .count();
            let contention = eligible + active.len() > 1;
            let candidates: Vec<_> = ready
                .iter()
                .enumerate()
                .filter_map(|(position, r)| {
                    let job = &jobs[r.id.index()];
                    if job.arrival > now {
                        return None;
                    }
                    let count = quantum(job, policy, contention);
                    let demand = request(r.id.index(), job, count);
                    resources
                        .can_acquire(&demand, &config, &admission)
                        .then_some((position, count, demand))
                })
                .collect();
            let force_bulk = streak >= 3
                && candidates
                    .iter()
                    .any(|(pos, _, _)| !ready[*pos].continuation);
            let chosen = candidates
                .into_iter()
                .filter(|(pos, _, _)| !force_bulk || !ready[*pos].continuation)
                .min_by_key(|(pos, count, _)| {
                    let r = &ready[*pos];
                    let job = &jobs[r.id.index()];
                    let age = now.saturating_sub(job.ready_at);
                    let urgent = matches!(policy, Policy::SoftDeadline)
                        && job.deadline.is_some_and(|deadline| {
                            now.saturating_add(*count as u64 * job.cost_ewma) >= deadline
                        });
                    // Aging wins over deadline urgency; continuation burst stays bounded.
                    (
                        age < AGE_LIMIT_US,
                        !urgent,
                        !r.continuation,
                        if urgent {
                            job.deadline.unwrap()
                        } else {
                            job.ready_at
                        },
                        r.id.index(),
                    )
                });
            let Some((pos, count, demand)) = chosen else {
                break;
            };
            let r = ready.remove(pos);
            max_wait = max_wait.max(now.saturating_sub(jobs[r.id.index()].ready_at));
            streak = if r.continuation { streak + 1 } else { 0 };
            assert!(graph.is_ready(r.id));
            assert!(resources.can_acquire(&demand, &config, &admission));
            resources.acquire(&demand);
            graph.mark_running(r.id).unwrap();
            let checkpoint = if jobs[r.id.index()].cursor > 0 {
                resume_us
            } else {
                0
            };
            resume_total += checkpoint;
            let run = count as u64 * ATOM_US + DISPATCH_US + checkpoint;
            max_run = max_run.max(run);
            active.push(Active {
                ready: r,
                resources: demand,
                count,
                started: now,
                end: now + run,
            });
            dispatches += 1;
            // Check slot occupancy independently of the admission counters.
            let mut occupancy = [0; CHANNELS];
            for running in &active {
                for (sum, uses) in occupancy.iter_mut().zip(held_channels(&running.resources)) {
                    *sum += usize::from(uses);
                }
            }
            assert!(occupancy.iter().all(|value| *value <= 1));
            assert!(resources
                .storage_reserved_bytes
                .values()
                .all(|bytes| *bytes <= 16));
        }
        host_ns += selection_started.elapsed().as_nanos();
        let next_end = active.iter().map(|run| run.end).min();
        let next_arrival = ready
            .iter()
            .map(|r| jobs[r.id.index()].arrival)
            .filter(|time| *time > now)
            .min();
        now = next_end
            .into_iter()
            .chain(next_arrival)
            .min()
            .expect("model admission deadlock");
        // Drain simultaneous finishes before the next admission wave.
        let mut i = 0;
        while i < active.len() {
            if active[i].end != now {
                i += 1;
                continue;
            }
            let running = active.remove(i);
            let index = running.ready.id.index();
            let job = &mut jobs[index];
            let checkpoint = if job.cursor > 0 { resume_us } else { 0 };
            let useful_start = running.started + DISPATCH_US + checkpoint;
            for (atom, phase) in job.atoms[job.cursor..job.cursor + running.count]
                .iter()
                .enumerate()
            {
                let start = useful_start + atom as u64 * ATOM_US;
                for (channel, uses) in phase.channels().into_iter().enumerate() {
                    if uses {
                        useful[channel] += ATOM_US;
                        intervals[channel].push((start, start + ATOM_US));
                    }
                }
            }
            for (channel, uses) in held_channels(&running.resources).into_iter().enumerate() {
                if uses {
                    held[channel] += running.end - running.started;
                }
            }
            resources.release(&running.resources);
            job.cursor += running.count;
            job.cost_ewma = (job.cost_ewma * 3 + ATOM_US) / 4;
            job.next_bound = (job.next_bound * 2).min(LARGE);
            let done = job.cursor == job.atoms.len();
            if done {
                job.finished_at = Some(now);
            } else {
                requeues += 1;
            }
            let finish_started = Instant::now();
            let newly_ready = graph
                .finish(
                    running.ready.id,
                    if done {
                        TaskRun::succeeded()
                    } else {
                        TaskRun::then(running.ready.task)
                    },
                )
                .unwrap();
            for r in &newly_ready {
                jobs[r.id.index()].ready_at = now.max(jobs[r.id.index()].arrival);
            }
            ready.extend(newly_ready);
            host_ns += finish_started.elapsed().as_nanos();
        }
    }
    let gaps = intervals.map(|mut spans| {
        spans.sort_unstable();
        let mut last = 0;
        let mut maximum = 0;
        for (start, end) in spans {
            maximum = maximum.max(start.saturating_sub(last));
            last = last.max(end);
        }
        maximum.max(now - last)
    });
    assert_eq!(graph.node_count(), jobs.len());
    assert_eq!(graph.summary().succeeded_nodes, jobs.len());
    assert_eq!(
        resources.cpu_in_use
            + resources.blocking_in_use
            + resources.network_in_use
            + resources.extract_in_use,
        0
    );
    assert!(
        resources.volume_reads.is_empty()
            && resources.volume_writes.is_empty()
            && resources.mutation_paths.is_empty()
            && resources.storage_reserved_bytes.is_empty()
    );
    let mut latencies: Vec<_> = jobs
        .iter()
        .map(|job| job.finished_at.unwrap() - job.arrival)
        .collect();
    let mut foreground: Vec<_> = jobs
        .iter()
        .filter(|job| job.foreground)
        .map(|job| job.finished_at.unwrap() - job.arrival)
        .collect();
    let mut background: Vec<_> = jobs
        .iter()
        .filter(|job| !job.foreground)
        .map(|job| job.finished_at.unwrap() - job.arrival)
        .collect();
    let deadline_misses = jobs
        .iter()
        .filter(|job| {
            job.deadline
                .is_some_and(|deadline| job.finished_at.unwrap() > deadline)
        })
        .count();
    let critical_unlock = jobs
        .iter()
        .filter_map(|job| {
            job.parent
                .map(|parent| job.finished_at.unwrap() - jobs[parent].finished_at.unwrap())
        })
        .max()
        .unwrap_or(0);
    Evidence {
        workload: name.into(),
        policy,
        model_us: now,
        p50_us: percentile(&mut latencies, 50),
        p95_us: percentile(&mut latencies, 95),
        foreground_p95_us: percentile(&mut foreground, 95),
        background_p95_us: percentile(&mut background, 95),
        max_non_arbitration_us: max_run,
        max_ready_wait_us: max_wait,
        scheduler_model_us: dispatches as u64 * DISPATCH_US,
        resume_model_us: resume_total,
        dispatches,
        requeues,
        deadline_misses,
        critical_unlock_us: critical_unlock,
        useful_utilization: useful.map(|time| time as f64 / now as f64),
        held_utilization: held.map(|time| time as f64 / now as f64),
        max_idle_gap_us: gaps,
        throughput_atoms_per_s: jobs.iter().map(|job| job.atoms.len()).sum::<usize>() as f64
            * 1_000_000.0
            / now as f64,
        capacity_violations: 0,
        scheduler_host_ns: host_ns,
    }
}

#[test]
fn work_unit_model_preserves_capacity_and_graph_identity() {
    for name in [
        "mixed",
        "throughput",
        "tiny",
        "costly-resume",
        "solo",
        "deadline-pressure",
    ] {
        for policy in POLICIES {
            let evidence = simulate(
                name,
                policy,
                if name == "costly-resume" { 1_000 } else { 40 },
            );
            assert_eq!(evidence.capacity_violations, 0);
            if !matches!(policy, Policy::Coarse) {
                assert!(evidence.max_non_arbitration_us <= LARGE as u64 * ATOM_US + 1_020);
            }
        }
    }
}
#[test]
fn small_quanta_can_lose_throughput() {
    assert!(
        simulate("throughput", Policy::FixedSmall, 40).model_us
            > simulate("throughput", Policy::Coarse, 40).model_us
    );
}
#[test]
fn deadline_cannot_admit_a_conflicting_resource_bundle() {
    let job = work(&[(Phase::Download, 1), (Phase::Extract, 1)], 0, true, None);
    let demand = request(0, &job, 2);
    let config = TaskPoolConfig {
        cpu_slots: 1,
        network_slots: 1,
        extract_slots: 1,
        ..TaskPoolConfig::default()
    };
    let mut state = ResourceState::default();
    state.acquire(&demand);
    assert!(!state.can_acquire(&demand, &config, &AdmissionSnapshot::default()));
    state.release(&demand);
    assert!(state.can_acquire(&demand, &config, &AdmissionSnapshot::default()));
}
#[test]
#[ignore = "prints Issue #1 deterministic model evidence; no production I/O"]
fn work_unit_model_evidence() {
    for name in [
        "mixed",
        "throughput",
        "tiny",
        "costly-resume",
        "solo",
        "deadline-pressure",
    ] {
        for policy in POLICIES {
            println!(
                "{}",
                serde_json::to_string(&simulate(
                    name,
                    policy,
                    if name == "costly-resume" { 1_000 } else { 40 }
                ))
                .unwrap()
            );
        }
    }
}

mod dispatcher_probe;

#[test]
fn continuation_does_not_unlock_a_dependent() {
    let mut builder = TaskGraph::builder();
    let parent = builder.add_root(task(0));
    let child = builder.add_task(task(1), [parent]).unwrap();
    let mut graph = builder.build();
    let ready = graph.start();
    assert_eq!(ready.len(), 1);
    graph.mark_running(parent).unwrap();
    let next = graph.finish(parent, TaskRun::then(task(0))).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].id, parent);
    assert!(!graph.is_ready(child));
    graph.mark_running(parent).unwrap();
    let next = graph.finish(parent, TaskRun::succeeded()).unwrap();
    assert_eq!(next[0].id, child);
}

#[test]
fn urgency_cannot_bypass_volume_path_or_storage_admission() {
    let config = TaskPoolConfig {
        default_volume_policy: VolumeIoPolicy::new(1, 1, 1, 2, VolumeStreamingMode::Mixed),
        ..TaskPoolConfig::default()
    };
    let admission = AdmissionSnapshot {
        storage_available_bytes: [("disk".into(), Some(10))].into_iter().collect(),
        ..AdmissionSnapshot::default()
    };
    let cases = [
        ResourceRequest {
            run: RunClass::AsyncIo,
            read_volumes: vec!["disk".into()],
            ..ResourceRequest::default()
        },
        ResourceRequest {
            run: RunClass::AsyncIo,
            write_volumes: vec!["disk".into()],
            ..ResourceRequest::default()
        },
        ResourceRequest {
            run: RunClass::AsyncIo,
            metadata_volumes: vec!["disk".into()],
            ..ResourceRequest::default()
        },
        ResourceRequest {
            run: RunClass::AsyncIo,
            mutation_paths: vec!["/output".into()],
            ..ResourceRequest::default()
        },
        ResourceRequest {
            run: RunClass::AsyncIo,
            storage_reservations: vec![StorageReservation {
                volume: "disk".into(),
                probe_path: "/unused-model-path".into(),
                bytes: 8,
            }],
            ..ResourceRequest::default()
        },
    ];
    for demand in cases {
        let mut state = ResourceState::default();
        assert!(state.can_acquire(&demand, &config, &admission));
        state.acquire(&demand);
        assert!(!state.can_acquire(&demand, &config, &admission));
        state.release(&demand);
        assert!(state.can_acquire(&demand, &config, &admission));
    }
}

# Task Pool Design

A frontend-neutral task pool drives install, update, verify, repair, VFS sync, and predownload apply. Public operations are decomposed into continuation tasks; one coordinator admits ready work only when every required resource permit is available.

---

## 1. Scheduling Model

The task pool has three fixed execution backends:

```text
resource-aware ready queue
├── Network workers   HTTP transfer futures submitted to compio
├── CPU workers       MD5, resume preparation, and source verification
└── Blocking workers  filesystem mutation and archive orchestration

small compio Dispatcher
└── native asynchronous network and filesystem completions
```

A slot is a permit, not a permanent task-specific operating-system thread. The scheduler no longer creates separate worker pools for general, archive, and VFS queues, and permanent queue loops do not occupy compio's blocking pool.

Every task is assigned a `ResourceRequest` containing the resources it actually competes for:

```rust
struct ResourceRequest {
    execution: ExecutionClass,
    network: Option<NetworkClass>,
    read_volumes: Vec<VolumeId>,
    write_volumes: Vec<VolumeId>,
    extract: bool,
    mutation_root: Option<InstallRootId>,
    estimated_bytes: u64,
}
```

A task starts only after all of its requested permits can be acquired atomically. This prevents CPU availability from being mistaken for disk availability.

---

## 2. Ready-Queue Priority and Fairness

Initial scan work enters the bulk queue. Tasks produced by completed work enter the continuation queue.

The scheduler admits up to three continuations before forcing a bulk admission when bulk work is runnable. This keeps repair, retry, and dependency-unblocking work near the critical path without starving a large initial verification scan.

Among runnable tasks in one priority class, admission considers:

1. five-second age buckets, so old work eventually wins;
2. physical-volume backlog, so congested volumes are drained deliberately;
3. estimated byte cost, preferring smaller work when age and backlog are equal;
4. original queue order as the final tie-breaker.

Network work uses one shared capacity pool. General, archive, and VFS transfers receive weighted opportunities in a `4:2:1` cycle, but unused capacity is borrowable by another class.

---

## 3. Physical-Volume Admission

All reads and writes are keyed by stable physical-volume identity rather than by task kind or path spelling.

The conservative default is:

```text
unknown or rotational media: 1 reader, 1 writer
```

A writer excludes readers on the same volume, and readers exclude writers. Per-volume overrides can be supplied with `TaskPoolConfig::with_volume_limit`, allowing known SSDs to use a higher read or write limit without increasing concurrency on unrelated HDDs.

Examples of work covered by the same volume policy include:

- full-file MD5 verification;
- partial-download prefix hashing;
- reuse-source verification;
- archive reads and staging writes;
- copy fallback;
- patch-base verification;
- archive commit and cleanup.

Install-root mutation additionally uses a root permit so patch, commit, delete, and cleanup operations cannot modify the same installation tree concurrently.

---

## 4. Download Flow

```text
Download / InstallArchivePart
    ├── inspect .part metadata
    ├── complete-size .part -> verify once
    │      ├── valid -> commit without HTTP
    │      └── invalid -> discard and restart
    └── partial .part -> hash existing prefix once
             │
             └── TransferDownload / TransferArchivePart
                    ├── shared HTTP client
                    ├── shared weighted network permit
                    └── destination-volume write permit
```

The CPU preparation task passes `DownloadResumeState { offset, hasher }` to the transfer task.

- `206 Partial Content`: continue writing and MD5 from the saved offset.
- `200 OK` after a Range request: truncate and restart from byte zero.
- `416 Range Not Satisfiable`: delete the stale partial file and retry without Range.

One long-lived `cyper::Client` is owned by the task-pool runner and cloned into transfer futures, preserving connection-pool and protocol state across files.

---

## 5. Verify, Repair, and Reuse

```text
Verify destination
    ├── valid -> Verified
    └── invalid -> RepairFile
                       ├── phase 1: same-volume hardlink candidates
                       │      └── first verified source atomically claims winner
                       ├── cancel remaining probes at hash-chunk boundaries
                       ├── phase 2: cross-volume copy candidates, only if phase 1 fails
                       └── phase 3: download, only if all local sources fail
```

Candidate order remains the caller's original order. It is not reordered by volume-key sorting.

The winner is claimed immediately rather than after every source volume finishes. A successful hardlink trusts the already-verified inode. Copy fallback hashes while copying and commits only after size and MD5 verification. A failed reuse operation re-enters repair so another source is verified before use.

---

## 6. Archive DAG

Archive work is no longer one coarse task containing nested thread pools.

```text
Extract (public request)
    ↓
PrepareArchive
    ├── inspect central directory once
    ├── create staging directory
    ├── build and persist patch plan
    └── split contiguous entry ranges
         ↓
ExtractArchiveShard × N
    ├── archive-volume read permit
    ├── staging-volume write permit
    └── shared extract permit
         ↓ all successful
CommitArchive
    ├── install-root mutation permit
    ├── staged commit, or dependency-ordered patch transaction
    └── schedule manifest follow-up where applicable
         ↓
CleanupArchive
    └── remove split volumes only after successful commit
```

Extraction shard completion uses a failure-aware fan-in barrier. The commit continuation is released only when every shard succeeds. The first shard failure is reported once; the remaining shards finish or observe cancellation state, and the staging directory is removed after the final shard exits.

Patch waves and staged commits run without hidden per-task thread fan-out. Parallelism belongs to the central scheduler, so configured permits match actual concurrency.

---

## 7. Safety and Failure Propagation

Each admitted task owns an RAII pending guard. Pending accounting is decremented even when a worker panics.

Task execution is wrapped with `catch_unwind` at the worker boundary. A panic produces a durable failure outcome instead of leaving `run_batch` waiting forever. Continuation enqueue failure also becomes a session failure and starts shutdown; it is never silently discarded.

Retries share one counter across preparation and transfer phases. Archive split-part fan-in releases extraction only after every required part succeeds.

---

## 8. Progress, Outcomes, and Metrics

`WorkerEvent` carries transient progress, retries, resets, and durable facts. `TaskPoolResult` retains durable `TaskOutcome` values and a `TaskPoolMetrics` snapshot.

The metrics snapshot contains:

- completed task count;
- queue-wait p50 and p95;
- task-duration p50 and p95;
- per-volume estimated read/write bytes;
- per-volume read/write task counts;
- accumulated read/write service time and derived bytes per second.

Estimated bytes come from manifest or task metadata. They are intended for comparative tuning and bottleneck diagnosis, not billing-grade byte accounting.

---

## 9. Configuration Defaults

Defaults are deliberately bounded rather than proportional without limit:

- compio dispatcher threads: `2..=4`;
- shared network slots: `4..=12`;
- CPU workers: `1..=12`;
- blocking workers: `2..=6`;
- simultaneous extraction transactions: `1..=2`;
- extraction shards per archive: `1..=4`;
- default volume policy: one reader and one writer.

Shared permits make concurrency resource-adaptive: idle network class capacity is borrowed, independent volumes progress concurrently, and a saturated volume blocks only tasks that need that volume.

---

## 10. Design Boundaries

Patch transaction ordering and recovery are documented in [`DESIGN_patch_pipeline.md`](DESIGN_patch_pipeline.md).

File allocation and Windows storage-reservation strategy are documented in [`DESIGN_optimizations.md`](DESIGN_optimizations.md).

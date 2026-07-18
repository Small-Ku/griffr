# Task Pool Design

A frontend-neutral task pool drives install, update, verify, repair, VFS sync, and predownload apply. Public operations are decomposed into continuation tasks; one coordinator admits ready work only when every required resource permit is available.

---

## 1. Scheduling Model

One coordinator owns the ready queues, admission state, pending count, progress reduction, and continuation scheduling. It does not execute task bodies itself and it does not spawn class-specific worker loops.

```text
resource-aware coordinator
├── acquire network / CPU / blocking / extract / volume / path permits
├── Dispatcher::dispatch()          async HTTP and compio file I/O
├── Dispatcher::dispatch_blocking() MD5, ZIP, HDIFF, sync filesystem work
└── receive TaskCompletion
      ├── release every acquired permit
      ├── update metrics and pending count
      ├── reduce progress/outcomes
      └── enqueue continuations
```

Every task is assigned a `ResourceRequest` containing the resources it actually competes for:

```rust
struct ResourceRequest {
    execution: ExecutionClass, // AsyncIo, Cpu, or Blocking
    network: Option<NetworkClass>,
    read_volumes: Vec<VolumeId>,
    write_volumes: Vec<VolumeId>,
    metadata_volumes: Vec<VolumeId>,
    extract: bool,
    mutation_root: Option<PathId>,
    estimated_bytes: u64,
    reuse_probe: bool,
    reuse_commit: bool,
}
```

A task starts only after all requested permits can be acquired atomically. `network_slots`, `cpu_slots`, and `blocking_slots` are coordinator admission limits rather than thread counts.

Async transfers execute on a Dispatcher runtime and return continuations via `TaskCompletion`. CPU and blocking tasks run in the Dispatcher's bounded blocking pool. If the pool transiently rejects a task, the coordinator restores it to the queue to avoid stalling other work.

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

*   **Writer Priority:** A streaming writer waiting over 15 ms reserves a streaming-pressure slot. In mixed mode, readers utilize remaining capacity; in exclusive mode, the reservation blocks new reads until the volume drains.
*   **Metadata Isolation:** Metadata mutations run in a separate lane. In mixed mode, they may overlap streaming tasks; in exclusive mode, they are isolated to prevent seek-heavy interleaving.

---

## 3. Physical-Volume Admission

All reads and writes are keyed by stable physical-volume identity rather than by task kind or path spelling.

The scheduler uses a `VolumeIoPolicy` per physical volume:
*   `read_limit` / `write_limit` / `metadata_limit`: Concurrency limits for each task type.
*   `streaming_pressure_limit`: Upper bound on concurrent read/write pressure.
*   `streaming_mode`: `Mixed` (overlapping operations) or `Exclusive` (mutual exclusion between reads, writes, and metadata mutations).

Per-volume overrides can be supplied via `TaskPoolConfig::with_volume_policy` (or CLI parameters like `--volume-read-limit`).

Examples of work covered by the policy include:
- full-file MD5 verification;
- partial-download prefix hashing and metadata commit;
- reuse-source verification;
- archive reads and staging writes;
- same-volume and cross-volume copy fallback;
- patch-base reads and output writes;
- archive commit, hardlink, delete, and cleanup operations.

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

Each Dispatcher runtime thread lazily reuses a thread-local `cyper::Client`. After download, `.part` files are promoted via async `compio::fs::rename`; copy fallbacks are dispatched as blocking jobs.

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

Reuse commits are pipelined (default limit: 16) to overlap verification and disk commits. `--relink-reuse` skips destination verification to replace existing valid files with hardlinks. Normal `verify --repair` verifies the destination first, querying reuse candidates only for missing or corrupt files.

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

Each task produces exactly one completion. Resource permits are released exclusively by the coordinator.

Task execution is wrapped with `catch_unwind` at the Dispatcher boundary. A panic produces a durable failure outcome. Rejected blocking submissions are restored to the queue; a stopped Dispatcher triggers a session failure.

Retries share one counter across preparation and transfer phases. Archive split-part fan-in releases extraction only after every required part succeeds.

---

## 8. Progress, Outcomes, and Metrics

`WorkerEvent` carries transient progress, retries, resets, and durable facts. `TaskPoolResult` retains durable `TaskOutcome` values and a `TaskPoolMetrics` snapshot.

The metrics snapshot contains:

- completed task count;
- queue-wait p50 and p95;
- task-duration p50 and p95;
- per-volume estimated read/write bytes;
- per-volume read/write/metadata task counts;
- accumulated read/write/metadata service time and derived streaming bytes per second.

Estimated bytes come from manifest or task metadata. They are intended for comparative tuning and bottleneck diagnosis, not billing-grade byte accounting.

---

## 9. Configuration Defaults

Defaults are deliberately bounded rather than proportional without limit:

- compio Dispatcher runtime threads: `2..=8`;
- shared network in-flight slots: `4..=12`;
- CPU admission slots: `1..=12`;
- blocking admission slots: `2..=8`;
- shared Dispatcher blocking-pool limit: CPU + blocking slots + 4 reserve slots, clamped to `8..=32`;
- simultaneous extraction transactions: `1..=2`;
- extraction shards per archive: `1..=4`;
- default volume policy: mixed I/O (4 readers, 2 writers, 2 metadata, pressure 6);
- reuse commit pipeline window: 16 files;
- write-reservation delay: 15 ms.

Shared permits make concurrency resource-adaptive: idle network class capacity is borrowed, independent volumes progress concurrently, and a saturated volume blocks only tasks that need that volume.

---

## 10. Design Boundaries

Patch transaction ordering and recovery are documented in [`DESIGN_patch_pipeline.md`](DESIGN_patch_pipeline.md).

File allocation and Windows storage-reservation strategy are documented in [`DESIGN_optimizations.md`](DESIGN_optimizations.md).

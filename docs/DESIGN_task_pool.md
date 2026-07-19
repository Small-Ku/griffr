# Task Pool and DAG Design

A frontend-neutral, command-scoped task DAG drives install, update, verify, repair, VFS sync, and predownload apply. The DAG describes ordering; the resource-aware scheduler decides when a ready node may run; `compio::Dispatcher` remains the only execution backend.

```text
planner -> TaskGraph -> resource-aware scheduler -> Dispatcher
                                               |-> dispatch()
                                               `-> dispatch_blocking()
```

---

## 1. Graph Model

`TaskGraphBuilder` constructs an append-only static DAG. A node may depend only on nodes already present in the builder, which rejects forward references and makes cycles unrepresentable. Duplicate dependency IDs are collapsed.

```rust
let mut graph = TaskGraph::builder();
let left = graph.add_root(left_task);
let right = graph.add_root(right_task);
let join = graph.add_task(join_task, [left, right])?;
let graph = graph.build_checked()?;
```

A node moves through:

```text
Pending -> Ready -> Running -> Succeeded
                         |-> Failed
Running -> Waiting -> Succeeded / Failed
Pending / Ready -> Cancelled
```

`Waiting` means the task body discovered and installed a dynamic subgraph. Its original node does not succeed until every terminal leaf of that subgraph succeeds.

The graph natively supports:

- fan-out: several nodes depend on one predecessor;
- fan-in: one node depends on several predecessors;
- dynamic expansion after a manifest, local hash, or archive inspection is known;
- descendant-only cancellation after failure;
- final graph metrics and per-node state inspection.

---

## 2. Dynamic Expansion

Executors return one `TaskExecution` value:

```text
Succeeded
Failed { reason, report }
Cancelled
Expand(GraphExpansion)
```

`GraphExpansion` is itself append-only and locally acyclic. The coordinator remaps its local node IDs into the command graph, marks the producer as `Waiting`, and attaches the producer to all terminal leaves.

Stable dependency tokens let a later expansion depend on a node installed by an earlier expansion. Archive volume tasks use this to expose verified byte ranges to central-directory, control-file, extraction-shard, and cleanup nodes without flattening the whole pipeline into one initial graph.

This replaces the former implicit `spawned.push(task)` continuation model. Dynamic expansion is used when the graph cannot be known up front:

- partial-download inspection selects resume, restart, or completion;
- verify failure selects repair;
- reuse probing selects hardlink, copy, another source, or download;
- archive inspection discovers extraction shards;
- a staged commit discovers manifest follow-up work.

Policy choices are resolved before adding work. Mutually exclusive alternatives are not represented as AND dependencies. For example, a matching reuse source creates only the chosen reuse branch; failed reuse may then expand another repair branch.

---

## 3. Scheduling Model

One coordinator owns graph state, ready queues, admission state, progress reduction, outcomes, and metrics. It does not execute task bodies and does not create class-specific worker loops.

```text
resource-aware coordinator
|- activate graph nodes whose dependency count reached zero
|- acquire network / CPU / blocking / extract / volume / path permits
|- Dispatcher::dispatch()          async HTTP and compio file I/O
|- Dispatcher::dispatch_blocking() MD5, ZIP, HDIFF, sync filesystem work
`- receive TaskCompletion
   |- release every acquired permit
   |- update metrics
   |- apply Succeeded / Failed / Expand to the graph
   `- enqueue newly ready nodes
```

Every ready task is assigned one `ResourceRequest`:

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

Dependency readiness and resource admission are deliberately separate. A node runs only when both are satisfied. `network_slots`, `cpu_slots`, and `blocking_slots` are admission limits, not custom thread counts.

Async transfers execute on Dispatcher runtimes. CPU and blocking work uses the Dispatcher's bounded blocking pool. A transient blocking-pool rejection restores the same graph node to the queue without losing its resource or dependency identity.

---

## 4. Ready-Queue Priority and Fairness

Initial roots enter the bulk queue. Nodes created by dynamic expansion enter the continuation queue.

The scheduler admits up to three continuations before forcing a bulk admission when bulk work is runnable. This keeps retries and dependency-unblocking work near the critical path without starving a large initial scan.

Within one priority class, selection considers:

1. five-second age buckets;
2. physical-volume backlog;
3. waiting-writer reservation;
4. metadata rank;
5. estimated byte cost;
6. original queue order.

General, archive, and VFS network tasks receive weighted opportunities in a `4:2:1` cycle, while unused capacity remains borrowable.

---

## 5. Physical-Volume Admission

Reads, writes, and metadata operations are keyed by stable physical-volume identity. A `VolumeIoPolicy` controls:

- `read_limit`, `write_limit`, and `metadata_limit`;
- `streaming_pressure_limit`;
- `streaming_mode`: `Mixed` or `Exclusive`.

Install-root mutation additionally uses a path permit, so commit, patch, delete, hardlink, and cleanup tasks cannot mutate the same target concurrently.

The graph does not encode these capacity constraints as edges. Doing so would make the graph machine-specific and would serialize work unnecessarily. The graph expresses correctness ordering; admission expresses current hardware capacity.

---

## 6. Download, Verify, Repair, and Reuse DAGs

Download preparation and transfer form a dynamic chain:

```text
Download
|- complete and valid --------------------------> success
|- resumable --------------------> TransferDownload
`- preparation failure and retry -> Download(next attempt)
```

A transfer failure expands another preparation node until the shared retry budget is exhausted.

Normal repair and explicit relink use conditional subgraphs:

```text
normal repair
Verify destination
|- valid -> success
`- invalid -> RepairFile

explicit relink
RepairFile
|- verified source -> ReuseFile
|                    |- hardlink success -> success
|                    |- hardlink failure -> copy fallback
|                    `- reuse failure -> another RepairFile
`- no source -> Verify destination -> optional Download
```

Reuse candidates are grouped by physical volume. Probes run as parallel DAG roots; the winning probe expands the selected commit node. The enclosing repair node waits for all probe terminals and the selected commit chain. Failed candidates do not release an unrelated download branch prematurely.

---

## 7. Range-Aware Archive DAG

Archive install no longer places one global barrier between all split-volume downloads and extraction. The package manifest first provides an immutable expected byte layout. Each volume download is bound to a stable dependency token, and tail volumes are queued first so archive planning can start on the critical path.

```text
InstallArchive
|- tail InstallArchivePart nodes first -----------------------|
|- remaining InstallArchivePart nodes continue in parallel   |
`- tail token(s) -> DiscoverArchiveDirectory                 |
                       |- EOCD / ZIP64 locator needs range ---+
                       `-> central-directory token(s)
                           -> InspectArchiveIndex
                           -> control-entry token(s)
                           -> ReadArchiveControls
                           -> PlanArchiveExtraction
```

`DiscoverArchiveDirectory` reads only the final EOCD search window. If a ZIP64 locator or end record crosses into an earlier unavailable volume, it returns the required byte range and dynamically creates another discovery node depending on exactly those volume tokens. Griffr accepts raw externally split ZIP streams and rejects true spanned/multi-disk ZIP metadata. Central-directory parsing then derives a conservative source range for every entry, from its local-header offset to the next local header or the central directory.

Extraction shards carry explicit entry lists and the union of only the source volumes those entries can touch:

```text
Verify volume 001 ----|
Verify volume 002 ----+-> Extract shard A (entries using 001..002) --|
                                                                    |
Verify volume 017 ----+-> Extract shard B (entries using 017) -------+-> shard join
                                                                    |
Verify volume 038 ----|
Verify volume 039 ----+-> Extract shard C (entries using 038..039) --|
```

This permits download and extraction overlap without reading a partial or unverified volume. The ranges intentionally over-approximate through the next local header so data descriptors, encryption headers, and alignment padding remain covered. Shards are ordered by their latest required volume and balanced by uncompressed bytes, which keeps early-volume work available without creating one node per ZIP entry.

The destructive-update barriers remain stricter than the data pipeline:

1. patch and delete control entries must be available and parsed;
2. patch transaction preflight completes before any shard mutates staging;
3. every extraction shard **and every package-part token** succeeds before `CommitArchive` becomes ready;
4. normal staged commit applies VFS and delete manifests in order;
5. archive cleanup remains downstream of commit and the manifest follow-up chain.

```text
required shard tokens -> extraction shards --|
                                              +-> CommitArchive -> manifest follow-up -> CleanupArchive
all archive volume tokens --------------------|
```

The all-volume join is intentionally placed at the irreversible commit boundary rather than at extraction. It preserves download/extraction overlap while preventing a late MD5 failure in an otherwise unused archive range from leaving a committed install.

A failed volume cancels only shards that need that volume and their commit descendant. Independent downloads and shards remain runnable. `ArchiveWork` owns the staging lifecycle and removes abandoned staging when the final graph references are dropped, covering cancellation paths where a shard never enters its executor. The small shared shard execution state remains only for first-error reporting and earlier cleanup after all actually started shards exit; it does not implement the commit barrier.

---

## 8. Failure and Cancellation

Each dispatched node produces exactly one completion. Resource permits are released only by the coordinator.

On failure:

- the node becomes `Failed`;
- its not-yet-running descendants become `Cancelled`;
- independent branches remain runnable;
- a dynamic parent waiting on the failed terminal becomes `Failed`;
- only failures with `report = true` create a durable `WorkerEvent::Failed` outcome.

Task execution is wrapped with `catch_unwind` at the Dispatcher boundary. A panic becomes a reported node failure. Stale queued entries belonging to cancelled nodes are discarded and their acquired permits are immediately released.

Cross-expansion token dependencies are checked before graph mutation. A child cannot depend on its expanding parent, a static descendant of that parent, or a dynamic ancestor currently waiting on it; those references are rejected as cycles instead of surfacing later as an admission deadlock.

The coordinator reports an admission deadlock when unresolved graph nodes remain but there is neither runnable/in-flight work nor a transiently full Dispatcher blocking pool.

---

## 9. Progress and Metrics

`WorkerEvent` remains the frontend-neutral stream for byte progress, retries, resets, and durable outcomes. DAG state is not encoded as renderer-specific callbacks.

`TaskPoolMetrics` contains scheduler timing and a `TaskGraphSummary`:

- total, pending, ready, running, waiting, succeeded, failed, and cancelled nodes;
- dynamic expansion count;
- completed dispatch count;
- queue-wait p50 and p95;
- task-duration p50 and p95;
- per-volume read/write/metadata counts, estimated bytes, and service time.

`run_tasks*` remains a convenience wrapper that turns a vector into independent root nodes. Callers that need explicit ordering use `TaskGraphBuilder` and `run_task_graph*` or `TaskPoolRunner::run_graph`.

---

## 10. Granularity and Boundaries

DAG nodes represent meaningful restartable work:

- one archive volume;
- one file verification, repair, or materialization;
- one extraction shard;
- one archive commit or manifest mutation.

Network chunks, read buffers, and individual hashing blocks are intentionally not nodes. Fine-grained byte processing stays inside one executor to avoid millions of scheduler entries.

Patch transaction ordering and recovery are documented in [`DESIGN_patch_pipeline.md`](DESIGN_patch_pipeline.md). File allocation and Windows storage strategy are documented in [`DESIGN_optimizations.md`](DESIGN_optimizations.md).

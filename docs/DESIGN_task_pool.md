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

Stable dependency tokens let a later expansion depend on a node installed by an earlier expansion. Archive volume tasks use this to expose verified byte ranges to central-directory, control-file, extraction-shard, and cleanup nodes without putting all work in one initial graph.

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
|- Dispatcher::dispatch()          async HTTP, compio file I/O, reuse copy, and delete manifests
|- Dispatcher::dispatch_blocking() MD5, ZIP, HDIFF, and filesystem tasks without async APIs
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

Async transfers, hardlink commits, verified reuse copies, and delete-manifest namespace actions run on Dispatcher runtimes. CPU and blocking work uses the Dispatcher's bounded blocking pool. A transient blocking-pool rejection restores the same graph node to the queue without losing its resource or dependency identity.

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

Reads, writes, and metadata steps are keyed by stable physical-volume identity. A `VolumeIoPolicy` controls:

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

Reuse candidates are grouped by physical volume. Probes run as parallel DAG roots; the winning probe expands the selected commit node. Both hardlink and copy commits use the async executor; copy commits stream with positional `compio::fs::File` I/O while verifying MD5 inline. The enclosing repair node waits for all probe terminals and the selected commit chain. Failed candidates do not release an unrelated download branch prematurely.

---

## 7. Lazy Range Archive DAG

Normal install and update no longer download complete `.zip.NNN` files before extraction. The launcher response supplies the immutable logical layout—ordered URLs, declared sizes, and package MD5 values—then the DAG fetches only the byte ranges required by the current plan or file-write step.

```text
InstallArchive
`- DiscoverArchiveDirectory
   `- Fetch tail range(s)
      `- Parse EOCD / ZIP64
         `- Fetch central-directory range(s)
            `- InspectArchiveIndex
               `- Fetch control-entry range(s)
                  `- ReadArchiveControls
                     `- patch/delete check
                        |- Fetch ranges for shard A -> Extract shard A
                        |- Fetch ranges for shard B -> Extract shard B
                        `- Fetch ranges for shard C -> Extract shard C
                                                   |- ephemeral -> CommitArchive
                                                   `- keep -> FetchMissingArchiveRanges
                                                              |- fetch volume gaps
                                                              `- SaveArchiveVolumes
                                                                  -> CommitArchive
                                                                      -> manifest follow-up
                                                                      -> CleanupArchive
```

`MultiVolumeLayout` maps one logical ZIP address space onto remote package volumes, retained complete local parts, and small cached range files. `MultiVolumeStream` implements `Read + Seek` over whichever backing segment contains the requested offset. The same EOCD, ZIP64, central-directory, and extraction code therefore serves complete local archives, production HTTP Range installs, and the ignored official sample test.

Range downloads are exact and resumable:

- Every request requires HTTP `206 Partial Content`;
- Incomplete ranges are cached as `*.range.part`;
- Retries resume from the existing partial length and request only the missing suffix;
- Completed segments are renamed atomically to `*.range`;
- Nearby requests on the same volume are coalesced to avoid small HTTP requests;
- Cache directory keys include package sizes and MD5s to prevent stale range reuse.

The directory planner first reads only the final EOCD search window. ZIP64 locator or end-record dependencies dynamically add preceding ranges. Central-directory parsing then derives a conservative source range for each entry—from its local header to the next local header or central directory—so encryption headers, data descriptors, and alignment padding remain available without exposing ZIP codec details to the scheduler.

Extraction shards preserve release frontiers and carry explicit entry lists. Frontiers are divided into compressed-source chunks (~256 MiB, unless a single entry is larger). Shards coalesce range requests internally and depend directly on their missing fetch nodes, allowing extraction to overlap with downloading without collapsing adjacent shards into full-volume requests.

```text
Fetch range 001:A ----|
Fetch range 002:B ----+-> Extract shard A --|
                                            |
Fetch range 017:C ------> Extract shard B --+-> CommitArchive
                                            |
Fetch range 038:D ----|                     |
Fetch range 039:E ----+-> Extract shard C --|
```

Each regular entry in the target `game_files` manifest is verified while written to staging:

1. Decompression completes and the ZIP CRC check passes;
2. Written size matches the manifest size;
3. Written MD5 matches the target file MD5;
4. Mismatching staged files are deleted and the archive range cache is invalidated;
5. Invalid cache data is removed after all graph references release the archive work.

Patch/control payloads not appearing in `game_files` are validated by ZIP CRC, patch transaction verification, and the command-level integrity DAG.

Mutation barriers remain strict during network/extraction overlap:

1. Patch and delete controls are parsed before staging work starts;
2. The destructive patch check completes before extraction shards run;
3. Every extraction shard must succeed before archive commit;
4. Ephemeral retention releases cached ranges after their final dependent shard completes;
5. Complete-volume retention preserves ranges, running `FetchMissingArchiveRanges` post-extraction for uncovered byte intervals;
6. `SaveArchiveVolumes` reconstructs original `.zip.NNN` files, verifies package MD5s, and atomically promotes them;
7. `CommitArchive` follows saved volumes in retention mode;
8. VFS/delete follow-up and cache cleanup remain downstream of commit;
9. The command-level integrity DAG verifies the final installation.

`ArchiveRetention` is the single policy switch across complete and range downloads:

- `Ephemeral`: Streams required ranges, releases them by shard lifetime, and cleans up residual archive data after commit.
- `KeepCompleteVolumes`: Retains range data, fills volume gaps post-extraction, verifies package MD5s, preserves `.zip.NNN` files, and removes the range cache.
- Predownload apply uses the same retention policy for local volumes.

Gap filling runs after all extraction shards complete to prevent archival completion traffic from competing with active extraction.

### Official archive format check

Synthetic tests cover raw byte splits, independent ZIP parts, split central directories, range-local extraction, spanned metadata rejection, and MD5 validation.

An ignored integration test checks the production remote source against the official Endfield package:

```powershell
cargo test -p griffr-common check_official_archive_sample -- --ignored --nocapture
```

It has no custom environment variables. It:

- Queries the launcher API;
- Caches ranges under `target/griffr-test-fixtures/archive-range-sample/<version>-<payload-id>`;
- Downloads EOCD tails, ZIP64/end records, central directories, and a bounded set of entry ranges;
- Fails if multiple volume boundaries expose standalone EOCD records;
- Compares central-directory paths with the decrypted `game_files` manifest;
- Extracts sampled entries via `MultiVolumeLayout` and `MultiVolumeExtractor`;
- Verifies output size and MD5 against the official manifest.

The ignored test checks for format changes without adding runtime support for independent or PKZIP-spanned archives.

---

## 8. Failure and Cancellation

Each dispatched node produces exactly one completion. Resource permits are released only by the coordinator.

On failure:

- the node becomes `Failed`;
- its not-yet-running descendants become `Cancelled`;
- independent branches remain runnable;
- a dynamic parent waiting on the failed terminal becomes `Failed`;
- only failures with `report = true` create a durable `WorkerEvent::Failed` outcome.

Task runs are wrapped with `catch_unwind` at the Dispatcher boundary. A panic becomes a reported node failure. Stale queued entries belonging to cancelled nodes are discarded and their acquired permits are immediately released.

Cross-expansion token dependencies are checked before graph mutation. A child cannot depend on its expanding parent, a static descendant of that parent, or a dynamic ancestor that is waiting on it; those references are rejected as cycles instead of surfacing later as an admission deadlock.

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
- one file check, repair, or write;
- one extraction shard;
- one archive commit or manifest mutation.

Network chunks, read buffers, and individual hashing blocks are intentionally not nodes. Fine-grained byte processing stays inside one executor to avoid millions of scheduler entries.

Patch transaction ordering and recovery are documented in [`DESIGN_patch_steps.md`](DESIGN_patch_steps.md). File allocation and Windows storage strategy are documented in [`DESIGN_optimizations.md`](DESIGN_optimizations.md).

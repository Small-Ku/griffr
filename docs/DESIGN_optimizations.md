# Resume, Reuse, Extraction, and Storage Optimizations

This design uses six code stages followed by packaging metadata.

## 1. Resume Recovery and Progress Reset

- `200` after a Range request truncates and restarts from byte zero.
- `416` deletes the stale partial and retries once without Range inside the same transfer attempt.
- `DownloadReset` replaces the per-path reducer maximum only for an explicit restart; ordinary progress remains monotonic.

## 2. Cross-Volume Reuse Routing

- Source and destination paths are compared by physical volume identity.
- Proven cross-volume candidates use `CopyOnly` routing and never issue a doomed hardlink action.
- Unknown identity keeps the hardlink-first fallback.

## 3. Write ZIP Volumes from Cached Ranges

- EOCD, ZIP64 records, and the central directory are fetched before payload data.
- `MultiVolumeLayout` presents retained local parts and cached HTTP ranges through one seekable stream.
- Range partials resume from their cached prefix instead of restarting a large segment after a transient failure.
- Cached compressed ranges carry shard-level lifetime tracking and are deleted when the last overlapping shard finishes, preventing double disk footprint during staging.
- `--keep-pack-archives` uses one re-entrant retention node per volume: after the last extraction reader, it fetches uncovered gaps at background priority, resumes itself, reconstructs the part, verifies package MD5, and promotes it.
- For lazy range archive DAG run details, see [`DESIGN_task_pool.md#7-lazy-range-archive-dag`](DESIGN_task_pool.md#7-lazy-range-archive-dag).

## 4. Peak Space and Forward Commit

- Patch plans calculate per-volume peak space for selected local and HDiff payloads before extraction starts.
- Full archives omit VFS and launcher metadata paths from extraction plans, ensuring single-writer ownership.
- Extraction shards verify and commit files inline, releasing staging space immediately without holding a full second installation.
- Deferred controls (`delete_files.txt`) remain private until all payload shards succeed, when `FinishArchive` publishes them.
- Successful destination checks pass to fallback integrity passes to skip re-verifying proven manifest entries.
- For task pool details, see [`DESIGN_task_pool.md`](DESIGN_task_pool.md).
- For patch apply details, see [`DESIGN_patch_steps.md`](DESIGN_patch_steps.md).

## 5. Cost-Aware Extraction Sharding

- Partition cost combines compressed source bytes, uncompressed hash/write bytes, compression-method CPU weight, and a fixed metadata cost per entry.
- Targets two ready shards per extraction slot; bounded by a 256 MiB compressed-source ceiling and a 512-entry cap per shard.
- Scheduler CPU ordering uses the same estimated cost that formed the shards.
- For shard run details, see [`DESIGN_task_pool.md#7-lazy-range-archive-dag`](DESIGN_task_pool.md#7-lazy-range-archive-dag).

## 6. Windows Allocation and Scheduler Reservation

- Download partials, archive ranges, ZIP entry outputs, cross-volume commits, and reuse copies reserve storage with `FILE_ALLOCATION_INFO` before streaming.
- The task scheduler tracks peak additional allocation per physical volume for each active writer, admitting new writers only when live free space covers active reservations.
- First writers use physical preallocation as the authoritative error point, allowing resume tasks to run on nearly full volumes.
- Read-only, verify, delete, base-release, hardlink, and metadata-only tasks do not consume byte reservations.
- Failed copy paths remove partial temporary outputs immediately.


## 7. Bounded Transfer and Scheduler Hot Paths

- HTTP response chunks are coalesced into 1 MiB write batches and passed through a bounded two-entry queue. Receiving and digesting the next batch can overlap the previous `write_all_at` without allowing unbounded body buffering.
- Download progress advances only after a batch has been persisted, so retries and resume offsets continue to describe durable bytes. The same writer serves ordinary downloads and archive-range cache files.
- The scheduler reuses one admission snapshot across a dispatch wave. Queue depth, queued reuse commits, writer reservations, and storage availability are updated incrementally as tasks leave the ready queues.
- Admission snapshots expire at the next pending writer-reservation deadline and are invalidated when work is enqueued, restored, or releases resources. Free-space queries are delayed until an active reservation or a newly selected first writer makes the value necessary.
- One command-scoped volume-key cache avoids repeating physical-volume identity probes when continuations or related tasks route the same path. Verify files share an exact parent-directory entry, so large flat manifest directories require one probe rather than one probe per file.
- Large ready sets enter a 256-1,024 node routed frontier sampled across async, CPU, and blocking classes. Workers therefore start before the scheduler resolves every manifest path, and queue-wait percentiles no longer include time spent outside the admission queue.
- Multi-target commands use a physical-volume dependency graph instead of barriered waves. Independent successors can start immediately, while one shared compio dispatcher prevents each target from creating another runtime and blocking pool.
- Archive range tables remain sorted when ranges are registered. Coverage and missing-range queries inspect only intersecting volumes under a read lock; local layouts refresh only those touched full files, while remote full volumes are registered when promotion succeeds.
- Archive streams retain the current file, volume, range, and local cursor. Sequential reads inside one segment therefore avoid repeated range-table locks, segment scans, and seeks.
- Archive repair compares candidate groups by uncached byte count without constructing concrete request vectors, then selects the smallest archive transfer that beats the direct download remainder.
- Download timeout environment values are resolved once per process, case-insensitive MD5 checks avoid temporary lowercase strings, and uncached verification avoids constructing artifact-cache keys.

## 8. Manifest-Driven Normal Updates

- Ordinary updates compare the locally committed `game_files` manifest with the target manifest before scheduling destination checks.
- Entries whose normalized path, expected MD5, and expected size are unchanged use a metadata-only existence/size check. They are not re-read solely to prove the same content expectation again.
- New entries and entries whose expected MD5 or size changed retain the strong MD5 destination check and the same verified repair/materialization fallback.
- Explicit `verify` and repair paths remain full integrity audits; this optimization is scoped to normal updates whose old and target canonical manifests agree about an entry.
- The split mirrors the observed YoStar launcher behavior documented in [`API_YOSTAR.md`](API_YOSTAR.md): normal updates trust unchanged manifest identity plus file metadata, while full repair hashes every desired file. Griffr remains stricter for newly produced content by hashing it before atomic commit.

## 9. Per-File Materialization Providers

- Manifest-driven ensure work treats compatible installation roots, full-package archive ranges, and the direct file CDN as providers for the same desired file rather than separate command modes.
- Reuse candidates remain strongly MD5-verified before hardlink/copy commit. If reuse cannot satisfy an entry, the task continues to network materialization.
- Full-package ZIPs are not downloaded as monolithic prerequisites. The existing archive session prepares central-directory metadata lazily, then compares each candidate entry's uncached compressed bytes with the remaining direct-download bytes at network admission.
- The archive provider is selected only when its missing compressed range is smaller than the direct transfer, so exposing full packs to manifest updates cannot force a larger archive download.
- Provider selection shares the command-scoped task pool and range cache; archive cache state is cleaned after the ensure batch unless archive retention is explicitly requested by a separate archive workflow.

## 10. Patch Archives as Group Delivery Candidates

- `Patch` and `Full` are archive package kinds, not update identities. A normal update remains defined by the old and target manifests.
- A compatible official patch can compete as one group-level delivery candidate when no reuse source is available. Its declared transfer bytes are compared with the changed-file direct-transfer upper bound and the declared full-package transfer.
- If the patch does not beat those bounds, the manifest route stays active and its per-file planner may independently choose reuse, ZIP ranges, or direct files.
- Explicit `--full-package`, staged predownload application, and archive-only fallback still select an archive package directly because those workflows explicitly require archive semantics.
- This keeps `pre_patch` useful for preloading while avoiding the previous `patch exists => patch pipeline` rule for ordinary updates.

## Validation

Validation uses `cargo fmt`, workspace `cargo check`, Clippy with warnings denied,
the Rust test suite, and the repository policy checker. Platform-specific I/O
behavior still requires final Windows verification because IOCP is the primary
runtime path.

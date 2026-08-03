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
- One command-scoped volume-key cache avoids repeating physical-volume identity probes when continuations or related tasks route the same path.

## Validation

Validation uses `cargo fmt`, workspace `cargo check`, Clippy with warnings denied,
the Rust test suite, and the repository policy checker. Platform-specific I/O
behavior still requires final Windows verification because IOCP is the primary
runtime path.

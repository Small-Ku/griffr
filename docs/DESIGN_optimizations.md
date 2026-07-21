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
- `MultiVolumeLayout` presents complete local parts and cached HTTP ranges through one seekable stream.
- Range partials resume from their cached prefix instead of restarting a large segment after a transient failure.
- Cached compressed ranges carry shard-level lifetime tracking and are deleted when the last overlapping shard completes, preventing double disk footprint during staging.
- `--keep-pack-archives` fills uncovered gaps after the last extraction reader for that volume, then reconstructs and verifies that part independently using background archive priority.
- For lazy range archive DAG execution details, see [`DESIGN_task_pool.md#7-lazy-range-archive-dag`](DESIGN_task_pool.md#7-lazy-range-archive-dag).

## 4. Peak-Space Model & Commit Batching

- A signed per-physical-volume ledger models extraction, parallel DAG outputs, cross-volume copies, external-work overlap, safe early deletes, consumed payload removal, and last-consumer base release.
- Normal commits use bounded batches (cross-volume up to 384 MiB, same-volume metadata serially per volume) with direct destination-verification successors (`VerifyCommittedBatch`).
- The fallback integrity pass receives successful destination checks to skip re-verifying manifest entries.
- For task pool DAG commitment and VFS integration, see [`DESIGN_task_pool.md`](DESIGN_task_pool.md).
- For patch transaction steps and entry DAG base release, see [`DESIGN_patch_steps.md`](DESIGN_patch_steps.md).

## 5. Cost-Aware Extraction Sharding

- Partition cost combines compressed source bytes, uncompressed hash/write bytes, compression-method CPU weight, and a fixed metadata cost per entry.
- Targets two ready shards per extraction slot; bounded by a 256 MiB compressed-source ceiling and a 512-entry cap per shard.
- Scheduler CPU ordering uses the same estimated cost that formed the shards.
- For shard execution details, see [`DESIGN_task_pool.md#7-lazy-range-archive-dag`](DESIGN_task_pool.md#7-lazy-range-archive-dag).

## 6. Windows Allocation Reservation

- Known-size download partials, ZIP entry outputs, cross-volume commits, and
  reuse copies reserve storage with `FILE_ALLOCATION_INFO` before streaming.
- Reservation preserves logical EOF and temporary-file atomicity.
- Failed copy paths remove their incomplete temporary output.
- `hdiffpatch-rs` owns creation of its first-pass codec output; griffr
  preallocates the destination-local verified copy when a work volume is used.

## Validation

The packaging environment has no Rust compiler or rustfmt and cannot resolve
the Rust distribution host. Validation therefore uses the repository's pinned
Tree-sitter checker, its Python regression tests, whitespace checks, patch
replay from the uploaded baseline, source-tree comparison, and extracted-ZIP
revalidation. Compiler-level `cargo fmt`, `cargo check`, `cargo clippy`, and
`cargo test` remain recommended on Windows after extraction.

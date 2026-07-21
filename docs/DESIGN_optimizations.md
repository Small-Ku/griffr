# Resume, Reuse, Extraction, and Storage Optimizations

This implementation is split into five code stages followed by documentation
and packaging metadata.

## 1. Resume Recovery and Progress Reset

- `200` after a Range request truncates and restarts from byte zero.
- `416` deletes the stale partial and retries once without Range inside the same
  transfer attempt.
- `DownloadReset` replaces the per-path reducer maximum only for an explicit
  restart; ordinary progress remains monotonic.

## 2. Cross-Volume Reuse Routing

- Source and destination paths are compared by physical volume identity.
- Proven cross-volume candidates are routed as `CopyOnly` and never issue a
  doomed hardlink operation.
- Unknown identity keeps the hardlink-first fallback.

## 3. Lazy Range ZIP Materialization

- EOCD, ZIP64 records, and the central directory are fetched before payload data.
- `MultiVolumeLayout` presents complete local parts and cached HTTP ranges through
  one seekable stream.
- Range partials resume from their cached prefix instead of restarting a large
  segment after a transient failure.
- Cached compressed ranges carry shard-level lifetime tracking and are deleted
  when the last overlapping shard completes, preventing double disk footprint
  during staging.
- Archive inspection owns the parsed `ZipArchive`; shards clone that metadata and
  fetch only overlapping ranges. Release frontiers are split into ~256 MiB compressed
  chunks to avoid monolithic download barriers.
- Files represented in `game_files` are size/MD5 verified while written to staging,
  before the atomic commit and final integrity pass.
- `--keep-pack-archives` reuses the lazy range DAG, fills uncovered volume gaps
  post-extraction, reconstructs original parts, and verifies package MD5s before preservation.

## 4. Dependency-Wave Peak-Space Model

- Preflight and execution use the same topological wave builder.
- A signed per-physical-volume ledger models extraction, parallel wave outputs,
  cross-volume copies, external-work overlap, safe early deletes, consumed
  payload removal, and last-consumer base release.
- Install, VFS, and work paths on one physical volume are checked once against
  the shared peak.

## 5. Windows Allocation Reservation

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

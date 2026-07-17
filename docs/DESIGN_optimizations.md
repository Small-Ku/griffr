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

## 3. Shared ZIP Metadata

- Archive inspection owns the initially parsed `ZipArchive`.
- Shards clone the inspected archive and shared volume layout rather than
  reparsing the central directory.
- Each shard keeps an independent stream cursor and contiguous entry range.

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

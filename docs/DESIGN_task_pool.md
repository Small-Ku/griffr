# Task Pool Design

## Overview

Install, update, verify, repair, VFS synchronization, and predownload apply use
one frontend-neutral task pool. Commands build different initial task lists, but
all work is expressed through shared task kinds, typed progress lanes, and
durable outcomes.

The pool separates network/file I/O, checksum work, and archive extraction so
each workload can be throttled independently. Windows I/O remains on the
compio/IOCP runtime; CPU-heavy checksum and extraction work use their dedicated
worker slots.

## Task Model

The current task surface is intentionally domain-oriented:

```rust
enum Task {
    InstallArchive { parts, dest, cleanup, password, patch_options, .. },
    Download { url, dest, expected_md5, expected_size, retry_count, .. },
    Verify { path, expected_md5, expected_size, on_fail, .. },
    EnsureFile { source_candidates, download_url, prefer_reuse, .. },
    Extract { volumes, dest, cleanup, password, patch_options, .. },
    ApplyExtractedVfsPatchManifest { install_root },
    ApplyDeleteManifest { install_root },
    Hardlink { src, dest },
}
```

`InstallArchive` is an archive state machine: verify each split volume, download
missing or mismatched parts, verify again, then spawn one `Extract` task.
`Verify.on_fail` and `EnsureFile` keep repair/reuse fallbacks inside the task
graph rather than in frontend-specific orchestration.

## Slot Groups

```text
shared task queues
├── I/O slots       Download, hardlink, copy, file writes
├── CPU slots       MD5 verification
└── Extract slots   ZIP parsing, extraction, HDiff materialization
```

The groups are independently limited:

- network concurrency should not be capped by CPU count;
- checksum concurrency should not exceed useful CPU parallelism;
- archive and patch work is disk- and CPU-heavy and must not starve downloads;
- materialization reserves enough I/O lanes to avoid nested-dispatch starvation.

## Archive Pipeline

An archive task performs these barriers:

1. Verify all split archive parts against API MD5 and size metadata.
2. Inspect archive paths and patch control files without mutating the install.
3. Build and validate a patch execution plan when `patch.json` is present.
4. Extract into a unique staging directory, optionally under `--work-dir`.
5. Either:
   - commit a normal staged extraction; or
   - execute the persisted forward-only patch transaction.
6. Delete archive parts only after successful completion when cleanup is
   enabled.

Normal extraction can cross volumes safely: files are copied into a
destination-local temporary file, verified, and atomically replaced. Patch
archives defer completion markers such as `config.ini` until VFS outputs and
cleanup have succeeded.

## Forward Patch Dependencies

The patch plan selects one exact source for each output before installation
mutation. An HDiff source records:

- base path, MD5, and size;
- patch payload path;
- destination path, MD5, and size.

The executor builds a destructive dependency graph. If entry B still consumes
the old contents of a path that entry A will overwrite, B runs before A. Every
output is generated into a temporary file, verified, and atomically committed.
A base listed for deletion is released only when its final consumer has
committed and it is not itself a final output.

`.griffr-patch/plan.json` is the forward-recovery source of truth. Resume skips
outputs already matching their target hash and revalidates both remaining
payloads and selected bases. The plan, extraction staging, and deferred files
are removed only after the target version marker commits.

## Runtime and Event Model

```text
compio dispatcher
├── I/O task bodies
├── CPU/extract worker dispatch
└── crate-private WorkerEvent stream
        ├── transient progress reduction
        └── durable TaskOutcome collection
```

`WorkerEvent` is crate-private. Byte samples, retries, and phase counters are
reduced while the batch is running and never retained in result history.
`TaskPoolResult` stores only durable `TaskOutcome` values such as verification,
download, reuse, extraction, preflight, and failure results.

## Progress Protocol

Cross-crate APIs do not accept renderer callbacks. `griffr-common` exposes a
cloneable `ProgressSender` and paired `ProgressReceiver`:

```rust
enum ProgressUpdate {
    Started { lane, unit, total },
    Advanced { lane, completed, total, item },
    Finished { lane },
    Failed { lane, item, reason },
}
```

A lane is a typed `(ProgressScope, ProgressPhase)` pair. Shared constants keep
verify, download, extract, commit, patch, delete, integrity, VFS, and
predownload streams distinct. `TaskProgress` maps worker facts to those lanes.

The CLI owns all `indicatif` state on a consumer thread. Interactive terminals
render stable rows; non-interactive stderr receives periodic textual samples.
A GUI can consume the same receiver without terminal dependencies in
`griffr-common`.

## Retry and Failure Rules

- Downloads carry a bounded retry count.
- Verification failure may enqueue one explicit repair/download task.
- A patch output is never committed before target MD5 and size verification.
- Selected HDiff bases are reverified immediately before patching.
- Missing staged payloads, invalid paths, dependency cycles, and insufficient
  known disk space fail before destructive patch work.
- Progress delivery failure does not change operation success or failure.

## Recovery Rules

`classify_patch_recovery` distinguishes archive-ready, resumable extracted,
incomplete, delete-pending, complete, and inconsistent states. `update`
automatically resumes a valid pending transaction before asking the API for a
new update. Explicit staged apply can replay verified archives when extracted
state is incomplete.

The model is forward-only: no rollback copy of the obsolete version is kept.
Correctness comes from persisted source selection, atomic per-output commit,
last-consumer base release, and a final deferred version marker.

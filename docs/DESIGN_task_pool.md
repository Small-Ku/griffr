# Task Pool Design

## Overview

Install, update, verify, repair, VFS synchronization, and predownload apply use
one frontend-neutral task pool. Commands build different initial task lists, but
all work is expressed through shared task kinds, typed progress lanes, and
durable outcomes.

The pool separates local checksum work, general file/network I/O, VFS CDN
traffic, and archive extraction so one throttle cannot accidentally constrain an
unrelated workload. Windows I/O remains on the compio/IOCP runtime; checksum
work uses dedicated CPU workers.

## Task Model

The current task surface is intentionally domain-oriented:

```rust
enum Task {
    InstallArchive { parts, dest, cleanup, password, patch_options, .. },
    Download { url, dest, expected_md5, expected_size, transfer_class, .. },
    Verify { path, expected_md5, expected_size, on_fail, .. },
    RepairFile { source_candidates, download_url, .. },
    ReuseFile { source, remaining_source_candidates, download_url, .. },
    Extract { volumes, dest, cleanup, password, patch_options, .. },
    ApplyExtractedVfsPatchManifest { install_root },
    ApplyDeleteManifest { install_root },
    Hardlink { src, dest },
}
```

`Task::ensure_file(FileEnsureTask)` builds the repair graph. Normal ensure starts
with `Verify`; only a failed verification enqueues `RepairFile`. Explicit relink
mode is the sole exception because its purpose is to replace an already-valid
destination with shared storage.

`InstallArchive` is an archive state machine: verify each split volume, download
missing or mismatched parts, then spawn one `Extract` task. Download streaming
already computes and validates MD5 and size, so a successfully committed part is
not read again solely for a second checksum pass.

## Repair and Reuse Flow

```text
Verify destination (CPU queue)
    ├── valid -> Verified
    └── invalid -> RepairFile (CPU queue)
                       ├── validate candidates until first usable source
                       │      └── ReuseFile (general I/O queue)
                       │             ├── hardlink -> trust verified inode
                       │             ├── copy -> hash while copying, then commit
                       │             └── I/O failure -> try next candidate
                       └── no source -> Download
```

The source search stops at the first valid candidate. Later candidates are
validated only if reuse of the earlier candidate fails. A successful hardlink
is not rehashed because it names the same already-verified file object. Copy
fallback computes MD5 while writing the temporary destination and commits only
when both size and MD5 match, avoiding a post-copy full-file read.

## Slot Groups

```text
shared task graph
├── General I/O slots  archive state, ordinary downloads, hardlink/copy, patch commit
├── VFS I/O slots      VFS CDN downloads only (default: 6)
├── CPU slots          destination MD5 and reuse-source validation
└── Extract slots      ZIP parsing, extraction, HDiff application
```

The groups are independently limited:

- local manifest verification always uses CPU workers, including `verify --repair`;
- the VFS limit applies only to `Download { transfer_class: Vfs }`;
- VFS hardlink/copy work uses general I/O slots, so local reuse is not restricted
  by the CDN safety limit;
- archive and patch work has its own throttle and cannot consume checksum slots;
- dispatcher capacity includes extra I/O lanes so worker loops waiting on nested
  compio operations do not starve completion work.

This prevents Endfield's conservative VFS concurrency from reducing all local
game-file verification to six workers.

## Archive Pipeline

An archive task performs these barriers:

1. Verify all existing split archive parts against API MD5 and size metadata.
2. Download invalid parts while computing MD5 during the write.
3. Inspect archive paths and patch control files without mutating the install.
4. Build and validate a patch execution plan when `patch.json` is present.
5. Extract into a unique staging directory, optionally under `--work-dir`.
6. Either commit a normal staged extraction or execute the persisted
   forward-only patch transaction.
7. Delete archive parts only after successful completion when cleanup is
   enabled.

Normal extraction can cross volumes safely: files are copied into a
destination-local temporary file, verified, and atomically replaced. Patch
archives defer completion markers such as `config.ini` until VFS outputs and
cleanup have succeeded.

Every committed archive file, patched VFS output, and delete-manifest path emits
a durable `Committed` outcome with its install-relative logical path. The update command uses this set
for incremental post-update integrity verification.

## Post-Update Integrity Scope

A normal explicit verify still selects the full `game_files` manifest.
Post-update verification selects only paths that the just-completed archive or
patch transaction committed, plus the separately planned VFS DAG. Files created
through the local-reuse update path are already validated by the ensure graph and
are not scanned again. Incremental manifest entries whose physical destination is
already covered by the VFS DAG are also removed, so the same file is not hashed
twice under different logical path conventions.

This avoids rereading an entire 80–100 GB installation after package-part and
patch-output validation have already established most of the relevant facts.
Users can still request a complete manifest scan through the explicit verify
command.

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
├── general I/O worker loops
├── VFS-download worker loops
├── CPU/extract worker loops
└── crate-private WorkerEvent stream
        ├── transient progress reduction
        └── durable TaskOutcome collection
```

`WorkerEvent` is crate-private. Byte samples, retries, and phase counters are
reduced while the batch is running and never retained in result history.
`TaskPoolResult` stores only durable `TaskOutcome` values such as verification,
download, reuse, extraction, changed-path, preflight, and failure results.

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
render stable rows; non-interactive stderr receives periodic textual samples. A
GUI can consume the same receiver without terminal dependencies in
`griffr-common`.

## Retry and Failure Rules

- Downloads carry a bounded retry count.
- Verification failure may enqueue one explicit repair task.
- A failed reuse operation validates the next source candidate before falling
  back to download.
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

# Task Pool Design

## Overview

Install, update, verify, repair, VFS synchronization, and predownload apply use
one frontend-neutral task pool. Commands build different initial task lists, but
all work is expressed through shared task kinds, typed progress lanes, and
durable outcomes.

The pool separates checksum preparation, general file/network I/O, VFS CDN
traffic, archive CDN traffic, extraction, patch application, and staged commit
work. Large operations are represented as bounded fan-out/fan-in graphs rather
than single tasks containing long serial loops. Windows I/O remains on the
compio/IOCP runtime; checksum work uses dedicated CPU workers.

## Task Model

The public task surface remains domain-oriented. Several hidden continuation
tasks represent scheduler-ready atomic work:

```rust
enum Task {
    InstallArchive { parts, dest, cleanup, password, patch_options, .. },
    InstallArchivePart { part, group, retry_count },          // CPU prepare
    TransferArchivePart { part, group, resume, retry_count }, // archive I/O

    Download { url, dest, expected_md5, expected_size, .. },  // CPU prepare
    TransferDownload { url, dest, resume, transfer_class, .. },

    Verify { path, expected_md5, expected_size, on_fail, .. },
    RepairFile { source_candidates, download_url, .. },
    VerifyReuseVolume { candidates, group, .. },
    ReuseFile { source, remaining_source_candidates, .. },

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

Hidden transfer tasks are not a second domain model. They are continuation
nodes carrying work already prepared by a CPU task, including an opaque
incremental MD5 state for a partial download.

## Download Preparation and Transfer

A download is split at the CPU/I/O boundary:

```text
Download / InstallArchivePart (CPU queue)
    ├── inspect .part metadata
    ├── complete-size .part -> MD5 once
    │      ├── valid -> commit without HTTP
    │      └── invalid -> discard and restart
    └── partial .part -> hash existing prefix once
             │
             └── TransferDownload / TransferArchivePart
                    (general, VFS, or archive I/O queue)
```

The preparation node passes `DownloadResumeState { offset, hasher }` to the
transfer node. The compio future therefore performs no synchronous prefix read.
If the server honors the Range request, streaming MD5 continues from the
prepared state. If the server replies with a full response, the transfer
truncates the partial file and starts a fresh hasher.

Download progress is emitted when either the configured byte threshold or a
short time threshold is reached, then one final sample is sent. The reducer
uses per-path monotonic maxima, so retries cannot move aggregate progress
backwards.

## Repair and Reuse Flow

```text
Verify destination (CPU queue)
    ├── valid -> Verified
    └── invalid -> RepairFile (CPU queue)
                       ├── canonicalize and deduplicate candidates
                       ├── group candidates by storage volume
                       ├── VerifyReuseVolume × volume (CPU queue)
                       │      └── candidates remain sequential within a volume
                       └── fan-in FirstUsableSource
                              ├── ReuseFile (general I/O queue)
                              │      ├── hardlink -> trust verified inode
                              │      ├── copy -> hash while copying, then commit
                              │      └── I/O failure -> next already-verified source
                              └── no source -> Download preparation
```

Different volumes can be read concurrently while one volume retains one
sequential reader, avoiding both unnecessary serialization and HDD seek storms.
A successful hardlink is not rehashed because it names the same already
verified file object. Copy fallback computes MD5 while writing the temporary
destination and commits only after size and MD5 match.

## Slot Groups and Bounded Inner Parallelism

```text
shared task graph
├── General I/O slots   ordinary transfers, hardlink/copy, manifest follow-up
├── VFS I/O slots       VFS CDN transfers only (default: 6)
├── Archive I/O slots   split archive transfers only (default: 6)
├── CPU slots           MD5, partial preparation, reuse-source validation
└── Extract slots       independent archive groups

bounded work inside one extract task
├── extract_shards      contiguous ZIP entry ranges (1..4)
├── patch_slots         independent patch entries per dependency wave (1..4)
└── commit_slots        staged-file commit jobs (1..8)
```

The groups are independently limited:

- local manifest verification always uses CPU workers, including
  `verify --repair`;
- the VFS limit applies only to prepared VFS transfer nodes;
- archive transfer limits do not restrict archive-part MD5 preparation;
- VFS hardlink/copy work uses general I/O slots;
- one archive may use several extraction shards without consuming several
  top-level extract slots;
- patch and commit inner concurrency is bounded separately;
- dispatcher capacity includes extra I/O lanes so worker loops waiting on nested
  compio operations do not starve completion work.

## Archive Fan-Out and Fan-In

An archive group no longer verifies and downloads all split volumes inside one
worker loop:

```text
InstallArchive
    ├── InstallArchivePart 001 (CPU) -> TransferArchivePart 001 (archive I/O)
    ├── InstallArchivePart 002 (CPU) -> TransferArchivePart 002 (archive I/O)
    ├── ...
    └── ArchiveInstallGroup barrier
             ├── any terminal failure -> cancel continuation
             └── all success -> Extract
```

Each part owns its retry chain. The barrier decrements only when a part either
succeeds or exhausts retries, so extraction cannot start from a partially
prepared volume set.

## Multi-Volume Layout and Extraction Shards

`MultiVolumeStream` records each volume's start/end offset and total length once
when opened. Seek locates the target volume by binary search instead of reading
metadata for every volume on every seek.

Archive inspection records entry sizes and the total uncompressed byte count.
Extraction reuses that inspection and divides contiguous entry ranges into a
bounded number of approximately byte-balanced shards:

```text
InspectArchive
    ├── ExtractShard [0..a)
    ├── ExtractShard [a..b)
    ├── ExtractShard [b..n)
    └── shard barrier -> staged commit / patch transaction
```

Each shard opens the archive once, processes a continuous index range, and
reuses one buffer. HDD-oriented configurations can keep one shard; NVMe systems
can use more without creating one scheduler task per ZIP entry.

## Staged Commit Fan-Out

The staging tree is traversed once to produce a deterministic `CommitFileJob`
list. Jobs execute in bounded chunks and report progress in list order after
each chunk completes. Cleanup runs only after all jobs succeed.

Same-volume jobs use replacement rename. Cross-volume jobs stream source bytes
to a destination-local temporary file while calculating MD5. Generic staging
commits retain one destination verification read because they have no expected
checksum; patch outputs compare the inline digest directly with their manifest
checksum and avoid a post-copy read.

## Patch Dependency Waves

Patch planning builds destructive dependencies before mutation. The executor
keeps that graph as waves instead of flattening it into one serial order:

```text
wave 0: entry A | entry B | entry C     (bounded by patch_slots)
                    barrier
wave 1: entry D | entry E
                    barrier
wave 2: entry F
```

Entries in one wave have no destructive dependency on one another. A wave must
finish successfully before the next wave begins. Deterministic callbacks,
last-consumer base release, and deferred completion-marker commit happen after
the corresponding wave results are joined.

A command-local `VerifiedArtifactCache` shares successful size/MD5 proofs
between patch preflight and apply. Its key includes path, expected size, and
expected MD5; its stamp includes observed size and modification time. It never
survives the command, and changed expected hashes cannot reuse an old proof.
Outputs generated and verified by one patch node can therefore feed downstream
nodes without rereading unchanged bases.

## Post-Update Integrity Scope

A normal explicit verify still selects the full `game_files` manifest.
Post-update verification selects only paths committed by the just-completed
archive or patch transaction, plus the separately planned VFS DAG. Files
created through the local-reuse update path are already validated by the ensure
graph and are not scanned again. Incremental manifest entries whose physical
destination is already covered by the VFS DAG are also removed.

This avoids rereading an entire 80–100 GB installation after package-part and
patch-output validation have already established the relevant facts. Users can
still request a complete manifest scan through the explicit verify command.

## Forward Patch and Recovery Rules

The patch plan selects one exact source for each output before installation
mutation. Every output is generated into a temporary file, verified, and
committed. A base listed for deletion is released only when its final consumer
has committed and it is not itself a final output.

`.griffr-patch/plan.json` is the forward-recovery source of truth. Resume skips
outputs already matching their target hash and revalidates remaining payloads
and selected bases. The plan, extraction staging, and deferred files are removed
only after the target version marker commits.

The model is forward-only: no rollback copy of the obsolete version is kept.
Correctness comes from persisted source selection, per-output commit,
last-consumer base release, dependency-wave barriers, and a final deferred
version marker.

## Runtime, Events, and Outcomes

```text
compio dispatcher
├── general I/O worker loops
├── VFS-transfer worker loops
├── archive-transfer worker loops
├── CPU and extract worker loops
└── crate-private WorkerEvent stream
        ├── transient progress reduction
        └── durable TaskOutcome collection
```

`WorkerEvent` is crate-private. Byte samples, retries, and phase counters are
reduced while the batch is running and never retained in result history.
`TaskPoolResult` stores only durable `TaskOutcome` values such as verification,
download, reuse, extraction, changed-path, preflight, and failure results.

Cross-crate APIs expose typed `ProgressSender`/`ProgressReceiver` wrappers, not
renderer callbacks or raw channels. The CLI owns terminal rendering; a GUI can
consume the same protocol.

## Retry and Failure Rules

- Preparation and transfer share one bounded retry counter.
- A failed transfer re-enters CPU preparation so the newest partial prefix is
  hashed once before the next request.
- Verification failure may enqueue one explicit repair graph.
- A failed reuse operation tries the next already-verified source before
  falling back to download.
- An archive barrier releases extraction only after every part succeeds.
- A patch wave failure prevents all later dependency waves.
- A patch output is never committed before target MD5 and size verification.
- Missing staged payloads, invalid paths, dependency cycles, and insufficient
  known disk space fail before destructive patch work.
- Progress delivery failure does not change operation success or failure.

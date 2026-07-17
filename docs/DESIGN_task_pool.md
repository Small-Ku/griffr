# Task Pool Design

A single frontend-neutral task pool drives install, update, verify, repair, VFS sync, and predownload apply. Works are expressed as a graph of task kinds, progress lanes, and durable outcomes.

---

## 1. Task Model

The public interface accepts domain tasks. Internally, the scheduler splits these into CPU and I/O tasks:

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

*   `Verify` failures enqueue `RepairFile`.
*   CPU tasks prepare state (e.g., hash prefix for resume) and pass it to downstream I/O tasks.

---

## 2. Download Flow (CPU / I/O Split)

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

*   The CPU task hashes the existing partial file once and passes `DownloadResumeState { offset, hasher }` to the I/O task.
*   If the CDN honors Range requests, streaming MD5 continues from the offset.
*   If the CDN ignores Range (returns `200`), the file is truncated and progress is reset.
*   If the CDN returns `416`, the stale partial file is deleted and retried once without Range headers.

---

## 3. Repair & Local Reuse Flow

```text
Verify destination (CPU queue)
    ├── valid -> Verified
    └── invalid -> RepairFile (CPU queue)
                       ├── Group candidates by storage volume
                       ├── VerifyReuseVolume × volume (CPU queue)
                       └── fan-in FirstUsableSource
                              ├── ReuseFile (general I/O queue)
                              │      ├── hardlink -> trust verified inode
                              │      ├── copy -> hash while copying, then commit
                              │      └── I/O failure -> try next verified source
                              └── no source -> Download preparation
```

*   **Volume Classification:** Candidates are grouped by physical disk volume.
*   **Concurrency:** Only one sequential reader executes per volume, avoiding HDD seek storms.
*   **Deduplication:** A successful hardlink does not require a re-hash. Copy fallback computes MD5 during stream copy and commits only on checksum match.

---

## 4. Slot Limits & Parallelism

```text
shared task pool
├── General I/O slots   ordinary transfers, hardlink/copy, manifest commit
├── VFS I/O slots       VFS CDN transfers only (default: 6)
├── Archive I/O slots   split archive transfers only (default: 6)
├── CPU slots           MD5 verification, resume prep, source verification
└── Extract slots       independent archive extractions (default: 1)

Extract tasks (inner parallelism)
├── extract_shards      contiguous ZIP entry ranges (1..4)
├── patch_slots         independent patch entries per dependency wave (1..4)
└── commit_slots        staged-file commit jobs (1..8)
```

*   VFS and archive downloads are isolated to prevent network starvation.
*   Inner loops (ZIP shards, patch slots, commits) are bounded independently.

---

## 5. Zip Extraction & Commit

*   **Binary Search Seek:** `MultiVolumeStream` tracks volume offsets once when opened. Seeks use binary search rather than reading metadata.
*   **Extraction Shards:** ZIP directory metadata is parsed once and shared. Contiguous index ranges are distributed to clones with separate stream cursors to extract in parallel.
*   **Staged Commit Fan-Out:** The staging tree is traversed to create a list of `CommitFileJob`s. Replacement rename is used for same-volume moves; cross-volume moves stream bytes while hashing.

---

## 6. Events & Outcomes

*   `WorkerEvent` handles real-time byte progress, retries, and resets.
*   `TaskPoolResult` stores only durable `TaskOutcome` values (e.g. verified files, download success, failures).
*   Progress is exposed via `ProgressSender`/`ProgressReceiver` wrappers.

---

## 7. Retry & Failure Rules

*   **Shared Retries:** Prep and transfer share a single retry counter.
*   **Failed Reuse:** Tries the next verified candidate; downloads only if all candidates fail.
*   **Barriers:** Archive extraction requires all split parts to succeed.
*   **Verification:** Patch outputs are verified before replacing the destination.

---

## 8. Patch & Optimization Design Boundaries

Detailed patch transaction flows, dependency ordering, and forward recovery plans are managed in [`DESIGN_patch_pipeline.md`](DESIGN_patch_pipeline.md).

For file allocation strategy (including Windows `FILE_ALLOCATION_INFO` and storage reservation), refer to [`DESIGN_optimizations.md`](DESIGN_optimizations.md).

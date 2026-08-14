# Griffr Architecture & Design Reference

This directory documents the core design, runtime task model, storage layouts, and patch batches of the `griffr` workspace.

The primary runtime is optimized for Windows large-file I/O, leveraging `compio` for asynchronous kernel I/O results (IOCP) and compio's blocking thread pool for CPU/blocking tasks.

The workspace has explicit dependency layers. `griffr-core` owns provider-neutral identity and value types. `griffr-hypergryph-api` and `griffr-yostar-api` each own one launcher protocol and may depend on core, but never on runtime or each other. `griffr-runtime` owns filesystem and execution policy and may consume both provider APIs. The CLI resolves user input into a provider-correct target before dispatching execution, so YoStar paths never manufacture Hypergryph channel/sub-channel values. `scripts/check_repo.py` enforces these dependency directions.

---

## File Index

| File | Contents |
|------|----------|
| [`DESIGN_compio.md`](DESIGN_compio.md) | Runtime analysis and rationale for selecting `compio` over `tokio` (Windows IOCP, zero-copy `IoBuf` streams, and HTTP/3 support). |
| [`DESIGN_task_pool.md`](DESIGN_task_pool.md) | Architecture of the unified frontend-neutral task pool, task graph compilation, slot groups, and structured progress/outcome reduction. |
| [`DESIGN_patch_steps.md`](DESIGN_patch_steps.md) | Steps for forward-only patch application, including asset storage, dependency waves, and archive checks. |
| [`DESIGN_resources.md`](DESIGN_resources.md) | Resource ownership, release snapshots, StreamingAssets closure, and Persistent working-set rules. |
| [`WORDING.md`](WORDING.md) | Project terms, direct naming rules, and exceptions for external API names. |
| [`DESIGN_account_model.md`](DESIGN_account_model.md) | Empirical analysis of the official launcher's LocalLow MMKV session caches and `griffr`'s file-based profile switching semantics. |
| [`DESIGN_optimizations.md`](DESIGN_optimizations.md) | Detail on optimization stages: resume recovery, cross-volume candidate reuse routing, shared ZIP index parsing, and Windows storage preallocation. |

---

## Quick Reference

### 1. Concurrency & Task Model

The workload separates asynchronous I/O and synchronous CPU tasks to prevent scheduler thread blocking and network starvation:

*   **I/O Queue (`compio`):** Run on a single-thread runtime driven by an I/O completion port (IOCP). Handles downloading, local copying, folder creation, and hardlink binding.
*   **CPU Queue (compio blocking pool):** Handles content-hash computation (MD5 for Hypergryph/Gryphline, CRC64-XZ for YoStar), zip entry parsing, and HDiff patch generation.
*   **Bridge:** Task bodies send thread-safe messages through bounded channels. Frontend clients read one progress protocol through the `ProgressReceiver` wrapper.

### 2. Patch Apply Rules

Griffr implements a forward-only update model with no rollback capability:

1.  **Check:** Scan the destination, verify inputs, and probe sources. Patch archives extract only selected payloads and estimate per-volume peak space before writing.
2.  **Persisted Patch Plan:** Save patch state to `.griffr/patch/plan.json` before file mutations start. Extract only selected payloads. Full archives recover directly from target manifests and range caches.
3.  **Verified Per-File Commit:** Write replacements to temporary paths, verify size and the backend-declared content hash, close handles, and replace target files atomically.
4.  **Dependency Release:** Release patch base files only after their last consumer node finishes.
5.  **Deferred Markers:** Write backend-native launcher metadata atomically after game-file verification and resource/delete follow-up. Hypergryph/Gryphline commit `config.ini` last; YoStar commits `manifest.json` before `game-launcher-config.json`, making the launcher config/version the final native metadata marker.

### 3. Private Install State and Change Marker

Griffr owns the `.griffr/` private directory inside each install:

```text
.griffr/
├─ state.json
├─ resource-baseline.json
├─ resource-baseline.pending.json
├─ persistent-vfs.json
├─ persistent-vfs.pending.json
├─ archives/
├─ patch/ (plan.json, deferred/)
└─ predownload/<from>-<to>/
```

`archives/` holds active package parts, `patch/` holds delta-patch recovery state, and `predownload/` holds future package sets.

Griffr writes `.griffr/state.json` before any file write starts. The marker records change type, game/channel identity, source/target versions, payload digests, resource policy, resource identity, release manifest identity, and start time. Resume commands read `.griffr/state.json` to resume interrupted work or advance to a new release manifest.

Griffr removes `.griffr/state.json` only after final integrity checks, backend-specific follow-up, and native launcher metadata commit succeed. This barrier is shared by Hypergryph/Gryphline and YoStar; an unfinished marker blocks `launch`.

Archive extraction, manifest loading, and delete plans reject paths inside `.griffr/`.

### 4. Unified Final-File Lifecycle

All write paths (archive extraction, patch apply, resource sync, reuse, repair) follow one final-file contract:

```text
resolve source -> create temp output -> verify size & content hash -> atomic replace -> emit ArtifactProof
```

- `ArtifactExpectation`: Target file path, size, and algorithm-tagged content hash.
- `ArtifactSource`: Byte origin (archive, CDN, patch, reuse, etc.).
- `ArtifactProof`: Destination path, size, source, and post-commit metadata stamp.
- `TaskOutcome::Committed { proof }`: Output returned by all successful file writers. Read-only checks return `TaskOutcome::Verified`.

Final integrity checks use `ArtifactProof` to skip re-verifying unmodified files written during the change. `ArtifactClaim` keeps provider ownership after tasks move into another DAG.

### 5. File Allocation & Storage

On Windows, output files reserve physical disk clusters with `FILE_ALLOCATION_INFO` before streaming:
*   Prevents fragmentation and repeated file system allocation updates.
*   Fails early on insufficient disk space before streaming large payloads.
*   Preserves logical EOF and atomic replacement semantics.

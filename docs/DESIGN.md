# Griffr Architecture & Design Reference

This directory documents the core design, runtime task model, storage layouts, and patch batches of the `griffr` workspace.

The primary runtime is optimized for Windows large-file I/O, leveraging `compio` for asynchronous kernel I/O results (IOCP) and compio's blocking thread pool for CPU/blocking tasks.

---

## File Index

| File | Contents |
|------|----------|
| [`DESIGN_compio.md`](DESIGN_compio.md) | Runtime analysis and rationale for selecting `compio` over `tokio` (Windows IOCP, zero-copy `IoBuf` streams, and HTTP/3 support). |
| [`DESIGN_task_pool.md`](DESIGN_task_pool.md) | Architecture of the unified frontend-neutral task pool, task graph compilation, slot groups, and structured progress/outcome reduction. |
| [`DESIGN_patch_steps.md`](DESIGN_patch_steps.md) | Steps for forward-only patch application, including VFS integration, dependency waves, and archive checks. |
| [`WORDING.md`](WORDING.md) | Project terms, direct naming rules, and exceptions for external API names. |
| [`DESIGN_account_model.md`](DESIGN_account_model.md) | Empirical analysis of the official launcher's LocalLow MMKV session caches and `griffr`'s file-based profile switching semantics. |
| [`DESIGN_optimizations.md`](DESIGN_optimizations.md) | Detail on optimization stages: resume recovery, cross-volume candidate reuse routing, shared ZIP index parsing, and Windows storage preallocation. |

---

## Quick Reference

### 1. Concurrency & Task Model

The workload separates asynchronous I/O and synchronous CPU tasks to prevent scheduler thread blocking and network starvation:

*   **I/O Queue (`compio`):** Run on a single-thread runtime driven by an I/O completion port (IOCP). Handles downloading, local copying, folder creation, and hardlink binding.
*   **CPU Queue (compio blocking pool):** Handles file hash computation (MD5 verification), zip entry parsing, and HDiff patch generation.
*   **Bridge:** Task bodies send thread-safe messages through bounded channels. Frontend clients read one progress protocol through the `ProgressReceiver` wrapper.

### 2. Patch Apply Rules

Griffr implements a **forward-only patch model** with no backward rollback capability:

1.  **Check:** Scan the destination, build dependencies, select sources, verify hashes, and estimate disk use.
2.  **Persisted Plan:** Save plan state to `.griffr-patch/plan.json` before starting mutations.
3.  **Entry DAG Processing:** Process changes as exact entry DAG nodes. Writers depend on consumers of the paths they replace; base files are released after their last consumer node finishes.
4.  **Deferred Markers:** Write configuration changes (`config.ini`) only after all VFS and staging directories are successfully processed and cleaned up.

### 3. File Allocation & Storage

On Windows, all output files with a known length reserve physical disk clusters via `FILE_ALLOCATION_INFO` before writing:
*   Prevents fragmentation and repeated file system allocation updates.
*   Triggers "out of disk space" errors at the start of the write sequence, rather than hours into a large stream.
*   Does not alter logical EOF, preserving standard atomic replace behaviors.

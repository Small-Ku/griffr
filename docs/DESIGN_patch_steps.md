# Patch Steps

This document describes the forward-only patch transaction implemented in `griffr-common`.

---

## 1. Scope & Normalization

A patch archive can contain standard replacements and a VFS payload (`patch.json`, `vfs_files/`, and `delete_files.txt`).

*   **Normalization:** Path fields (`local_path`, `base_file_path`, `patch_path`) normalize to defaults if empty.
*   **Safety:** Paths containing drive prefixes, absolute indicators, or parent directory traversals (`..`) are rejected during deserialization.

---

## 2. Predownload Metadata

`predownload fetch` persists `.griffr-predownload.json` alongside split archive parts:
*   Records game identity, channel, source/target versions, and archive file hashes.
*   Prevents incorrect versions from being applied based on directory names alone.

---

## 3. Archive Check and Patch Plan

Before mutating the installation:
1.  Validates entry paths and checks for duplicates.
2.  Ensures planned HDiff base files exist and match target size/MD5.
3.  Simulates disk space requirements for extraction, staging, and deletions.
4.  Saves the planned sources to `.griffr-patch/plan.json`.
5.  Fails early if free space on the destination volume is insufficient.

Apply and resume phases consume `plan.json` directly to avoid re-calculating sources after mutation has begun.

---

## 4. Forward-Only Transaction Flow

Rollbacks are not supported. The transaction runs forward:

1.  Write `.griffr-patch/plan.json`.
2.  Prepare VFS folder/links.
3.  Commit top-level files; write deferred markers (e.g., `config.ini`) to `.griffr-patch/deferred`.
4.  Remove files marked for deletion (unless needed as active patch bases).
5.  Apply patches in wave-dependency order to temporary files, verify MD5, and atomically rename.
6.  Delete a base file as soon as its last consumer wave commits.
7.  Commit deferred markers.
8.  Clean up staging directories and plan files.

---

## 5. Dependency Waves

To prevent a patch output from overwriting a file that is still needed as a base for another patch, planning groups work into destructive dependency waves:

```text
wave 0: entry A | entry B     (Run concurrently)
             barrier
wave 1: entry C               (Overwrites A's base file)
```

*   Consumers run in earlier waves before writers.
*   If a dependency cycle is detected, the transaction fails before step work begins.
*   A command-local `VerifiedArtifactCache` prevents redundant base checks between waves.

---

## 6. Staging, Work Directory, & VFS Links

*   **Work Directory:** When using `--work-dir`, staging and HDiff temporary files are created outside the install root. Outputs are verified in the work directory, copied local to the install volume, verified again, and committed.
*   **External VFS Root:** `--external-vfs-root` moves the VFS folder to an external target and symlinks it. Details are stored in `.griffr-storage.json`. Verification and repair commands follow the link.
*   **Codecs:** Patches use `HDIFFSF20` format, applied via `hdiffpatch-rs`.

---

## 7. Crash Recovery

`get_patch_recovery_state` selects the recovery path at startup:
*   If a plan exists, the update skips successfully matched files, re-verifies bases, and resumes work.
*   The transaction is marked complete only after staging cleanup and deferred version-marker commits succeed.

---

## 8. File Preallocation Reference

For details on the physical file allocation strategy on Windows (including `FILE_ALLOCATION_INFO` and temporary-file preallocation), refer to [`DESIGN_optimizations.md`](DESIGN_optimizations.md).

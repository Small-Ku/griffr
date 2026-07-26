# Patch Steps

This document describes the forward-only patch step implemented in `griffr-common`.

---

## 1. Scope & Normalization

A patch archive can contain standard replacements and a VFS payload (`patch.json`, `vfs_files/`, and `delete_files.txt`).

*   **Normalization:** Path fields (`local_path`, `base_file_path`, `patch_path`) normalize to defaults if empty.
*   **Safety:** Paths containing drive prefixes, absolute indicators, or parent directory traversals (`..`) are rejected during deserialization.

---

## 2. Predownload Metadata

`predownload fetch` stores each package set under `.griffr/predownload/<from>-<to>/` and persists `.griffr-predownload.json` alongside the split archive parts:
*   Records game identity, channel, source/target versions, and archive file hashes.
*   Prevents incorrect versions from being applied based on directory names alone.

---

## 3. Archive Check and Patch Plan

Before mutating the installation:
1.  Validates entry paths and checks for duplicates.
2.  Probes final outputs and candidate HDiff bases, then selects exactly one source per entry.
3.  Builds a source-directed extraction set containing ordinary top-level files, deferred markers, and only the selected local/HDiff payloads.
4.  Simulates disk space for that selected extraction set, patch output waves, staging, and deletions.
5.  Saves the selected sources to `.griffr/patch/plan.json` before any archive entry changes the installation.
6.  Fails early if free space on the destination volume is insufficient.

Apply and resume phases consume `plan.json` directly to avoid re-calculating sources after mutation has begun. The command-level `.griffr/state.json` marker wraps this patch plan and keeps the target version visible if the process stops before final integrity and metadata sync.

---

## 4. Forward-Only Patch Flow

Rollbacks are not supported. The patch runs forward:

1.  Write `.griffr/patch/plan.json`.
2.  Extract only selected patch payloads to private staging. Unused alternate diffs and payloads for already-valid outputs are skipped entirely.
3.  Extract ordinary top-level files to private temporary paths, verify them, and commit each file immediately. Deferred markers such as `config.ini` remain private.
4.  Prepare VFS folder/links and move deferred markers to `.griffr/patch/deferred`.
5.  Remove files marked for deletion unless they remain active patch bases.
6.  Apply each patch entry as a DAG node. Local payloads are verified before destination replacement; HDiff outputs are generated to temporary paths, verified, and atomically renamed. Each entry returns an `ArtifactProof`; an already-correct output returns an `Existing` proof without reporting a mutation.
7.  Delete a base file after all of its consumer nodes finish.
8.  Commit deferred markers.
9.  Clean up staging directories and plan files.

---

## 5. Entry DAG

To prevent a patch output from overwriting a file that another patch still needs as a base, the runner creates exact entry dependencies:

```text
PreparePatchApply
  |- ApplyPatchEntry A ---\
  |- ApplyPatchEntry B ---+-> ReleasePatchBase X
  `- ApplyPatchEntry C ---> ApplyPatchEntry D (replaces C's base)
                              |
       all entry/base leaves -+-> ApplyPatchDeletes
                              `-> CommitPatchDeferred
                              `-> CleanPatchApply
```

*   A writer depends only on the consumers of the path it replaces; unrelated entries do not share a wave-wide join.
*   Each entry declares its base and payload reads, output/work writes, and destination mutation path to the scheduler.
*   Path mutation locks reject equal, ancestor, and descendant conflicts while allowing unrelated files to run together.
*   If a dependency cycle is detected, the patch fails before step work begins.
*   A command-local `VerifiedArtifactCache` prevents redundant base checks across entry nodes.
*   Dependency waves remain as a derived view for peak-space simulation and the serial recovery fallback; they no longer control normal archive commit runs.

---

## 6. Staging, Work Directory, & VFS Links

*   **Source-Directed Staging:** Patch archives do not extract the full `vfs_files/` tree. The selected plan determines the exact local and HDiff payload paths needed by the run; all alternatives remain compressed and unfetched when range delivery is used.
*   **Work Directory:** When using `--work-dir`, selected payload staging and HDiff temporary files are created outside the install root. Outputs are verified in the work directory, copied local to the install volume, verified again, and committed.
*   **External VFS Root:** `--external-vfs-root` moves the VFS folder to an external target and symlinks it. Details are stored in `.griffr-storage.json`. Verification and repair commands follow the link.
*   **Codecs:** Patches use `HDIFFSF20` format, applied via `hdiffpatch-rs`.

---

## 7. Crash Recovery

`get_patch_recovery_state` selects the recovery path at startup:
*   If a complete plan, selected payload set, and deferred marker set exist, the update skips successfully matched files, re-verifies bases, and resumes work.
*   If source-directed extraction stopped before all selected payloads or deferred markers arrived, normal `update` removes only the private staging/plan state and rebuilds it from the current archives. Already committed files remain and are selected as `AlreadyPresent` by the rebuilt plan.
*   The patch is marked done only after staging cleanup and deferred version-marker commits succeed.

---

## 8. File Preallocation Reference

For details on the physical file allocation strategy on Windows (including `FILE_ALLOCATION_INFO` and temporary-file preallocation), refer to [`DESIGN_optimizations.md`](DESIGN_optimizations.md).

# Patch Pipeline

This document describes the patch archive behavior currently implemented in `griffr-common`.

## Scope

The verified archive behavior is based on real launcher patch payloads inspected on `2026-07-10`:

- Arknights `74.0.0 -> 75.0.0`
- Endfield `1.2.5 -> 1.3.3`

The important finding is that the VFS patching sub-payload is shared across both games even though the full patch archives are not identical.

## Archive Shape

A full patch archive may contain both of these categories at the same time:

1. Top-level replacement files
   - Examples: `Endfield.exe`, `GameAssembly.dll`, plugin DLLs, index JSON files
   - These are committed by generic archive extraction before any patch-manifest-specific logic runs.
2. Extracted VFS patch manifest payload
   - `patch.json`
   - `vfs_files/files/...`
   - `vfs_files/vfs_patch/...`
   - `delete_files.txt`

`patch.json` is the control file for the VFS-specific follow-up stage. It is not game-specific in the current implementation.

## Shared VFS Manifest Format

The current common code expects the following fields from `patch.json`:

- `vfs_base_path`
- `files[]`
- `files[].name`
- `files[].md5`
- `files[].size`
- `files[].local_path`
- `files[].patch[]`
- `files[].patch[].base_file`
- `files[].patch[].base_file_path`
- `files[].patch[].base_md5`
- `files[].patch[].base_size`
- `files[].patch[].patch`
- `files[].patch[].patch_path`

The materialization destination is `install_root / vfs_base_path / files[].name`.

## Runtime Order

The task-pool pipeline is intentionally split into three phases:

1. `Task::Extract`
   - Extract the archive into a staging directory.
   - Commit staged files into the install root.
2. `Task::ApplyExtractedVfsPatchManifest`
   - Parse `patch.json`.
   - Move direct `local_path` payloads from `vfs_files/files/...` into the final VFS tree.
   - Apply `HDIFFSF20` diffs from `vfs_files/vfs_patch/...` against verified base files.
   - Verify each materialized output by MD5 and size.
   - Remove `patch.json` and `vfs_files` only after success.
3. `Task::ApplyDeleteManifest`
   - Apply `delete_files.txt` relative to the install root.
   - Remove `delete_files.txt` after success.

This ordering matters:

- top-level replacement files must already be committed before VFS patch materialization
- `delete_files.txt` must run after VFS patch materialization so extracted patch inputs are still available while patching

## Path Rules

All manifest-driven relative paths are validated before use:

- empty paths are rejected
- absolute paths are rejected
- drive-prefixed paths are rejected
- parent traversal (`..`) is rejected

This applies to:

- `delete_files.txt`
- `patch.json` `vfs_base_path`
- `patch.json` `name`
- `patch.json` `local_path`
- `patch.json` `base_file` / `base_file_path`
- `patch.json` `patch` / `patch_path`

## Current Codec Assumption

The VFS diff files inspected from both games use `HDIFFSF20`. The implementation applies them through `hdiffpatch-rs`.

This is only the VFS diff stage. It does not imply that every file in a patch archive is diff-based.

## Non-Goals

The extracted VFS manifest stage does not:

- decide whether a patch archive should be downloaded
- inspect launcher availability rules
- handle generic top-level archive file moves differently per game

Those concerns stay outside this follow-up stage.

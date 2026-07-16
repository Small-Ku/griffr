# Patch Pipeline

This document describes the forward-only patch transaction implemented in
`griffr-common`.

## Scope

The archive shape was validated against real Arknights and Endfield launcher
patches. A patch archive can contain ordinary top-level replacements together
with a VFS-specific payload:

- `patch.json`
- `vfs_files/files/...`
- `vfs_files/vfs_patch/...`
- `delete_files.txt`

`patch.json` is the control file for the VFS transaction. The patch-output
destination is `install_root / vfs_base_path / files[].name` unless the user
selects an external VFS root.

## Manifest Path Semantics

The common model accepts:

- `files[].local_path`
- `files[].patch[].base_file`
- `files[].patch[].base_file_path`
- `files[].patch[].patch`
- `files[].patch[].patch_path`

Optional path fields are normalized at deserialization time. Missing, empty,
or whitespace-only alternate fields fall back to their primary field. Every
effective path still passes the shared safe-relative-path validator; absolute,
drive-prefixed, empty, and parent-traversal paths are rejected.

## Predownload Stage Metadata

`predownload fetch` writes `.griffr-predownload.json` beside the split archive
parts. It records:

- game, region, channel, and sub-channel identity;
- source and target versions;
- every archive filename, size, and MD5;
- the metadata schema version and creation timestamp.

Historical apply never infers a transition solely from a directory name. It
loads the metadata, requests patch information for the recorded source version,
requires that the live response still targets the recorded target version, and
verifies the staged archive identity before extraction.

## Preflight

Before extraction mutates the install, the archive is inspected without
applying its contents. Preflight:

1. validates every ZIP entry path and rejects duplicates;
2. parses `patch.json` and `delete_files.txt`;
3. chooses one exact usable source for every VFS output;
4. verifies that each selected base currently matches its expected size and MD5;
5. verifies that every selected local or HDiff payload exists in the archive;
6. rejects multiple writers, output/delete conflicts, unsafe paths, and invalid
   external storage topology;
7. estimates final growth plus install, VFS, and work-volume peak requirements;
8. fails before extraction when a known volume does not have enough free space.

The selected sources, including each HDiff base MD5 and size, are persisted
in `.griffr-patch/plan.json`. Apply and resume consume that plan instead of
selecting candidates again after mutation has started.

## Forward-Only Destructive Transaction

Old versions are not retained for rollback. The transaction is recoverable
forward to the target version:

1. Persist the schema-v2 execution plan before committing staged files.
2. Prepare the optional external VFS root.
3. Commit ordinary top-level files, but move deferred completion markers such
   as `config.ini` into `.griffr-patch/deferred`.
4. Delete manifest paths that are not patch bases or final outputs.
5. Process VFS entries in dependency order.
6. For each entry:
   - generate into a temporary output;
   - verify target MD5 and size;
   - atomically replace the destination;
   - remove the consumed patch payload;
   - decrement the selected base's consumer count;
   - remove an obsolete base as soon as its last consumer has committed.
7. Apply any remaining non-conflicting delete paths.
8. Commit deferred version markers last.
9. Remove extraction staging and transaction metadata.

A base that is also a final output is never deleted after replacement. A delete
manifest that directly conflicts with a planned output is rejected during
preflight.

## Dependency Ordering

A patch output may overwrite a file that another entry still needs as a base.
The plan therefore builds a destructive dependency graph. Consumers of an old
base run before the writer that replaces that path. Cycles are rejected rather
than risking irreversible source destruction.

This replaces the former global "keep every old file, then run the whole delete
manifest last" rule. Delete-only paths can be released early, while each base
remains protected until its final consumer succeeds.

## Crash Recovery

`classify_patch_recovery` distinguishes:

- archive-ready staged downloads;
- a persisted transaction that can resume;
- incomplete extracted state with missing payloads or bases;
- legacy delete-only state;
- clean completion;
- inconsistent control state.

`predownload resume` reopens `.griffr-patch/plan.json`, skips outputs that
already match their target hash, revalidates remaining selected bases, and
continues the same selected plan. A normal `update` performs the same recovery
check automatically before requesting another update. The plan
is removed only after staging cleanup and deferred version-marker commit both
succeed.

## Work Directory

`update --work-dir` and `predownload apply --work-dir` place extraction staging
and HDiff temporary output outside the install root. HDiff output produced on a
different volume is verified there, copied to a destination-local temporary
file, verified again, and then atomically committed.

The work directory must not be inside the install root and must not overlap the
external VFS root.

## External VFS Root

`--external-vfs-root` moves the VFS tree to a persistent external directory and
links the logical VFS path into the install. The target must:

- be outside the install root;
- differ from the logical VFS path;
- be empty unless it is already the recorded link target.

The topology is stored in `.griffr-storage.json`. Verification and repair use
the logical install path and therefore follow the link. Uninstall reads the
metadata and removes both the install root and its managed external VFS root.

## Progress

The task pool emits separate archive verify, download, extract, commit, patch,
and delete lanes. Preflight reports are durable outcomes and are rendered by
the frontend. Interactive terminals use progress bars; non-interactive stderr
receives periodic textual samples so long patch operations do not appear hung.

## Codec Assumption

The inspected VFS diffs use `HDIFFSF20` and are applied with `hdiffpatch-rs`.
Every generated output is verified before destination replacement.

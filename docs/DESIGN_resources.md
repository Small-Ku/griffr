# Resource File Design

This document defines how install, update, verify, and Persistent resource setup handle launcher resource files.

---

## 1. Resource Sources

Griffr uses two independent manifest sources:

- `game_files` describes the release file set from the game package service.
- `index_initial.json` and `index_main.json` describe the launcher resource baseline under `StreamingAssets`.

The `auto` resource policy calls `get_latest_resources` without hard-coded parameters and handles HTTP 400 (`resource not exist`) responses.

The `package-only` policy skips `get_latest_resources`. Package archives and `game_files` manage resource files.

`--resources package-only` selects package-only handling on install and update. The hidden legacy `--skip-vfs` alias remains for compatibility; it does not filter resource entries inside packages or `game_files`.

---

## 2. One Release Snapshot

`GameManifestSnapshot` fetches and validates `game_files` once per command. `ContentPlan` merges that snapshot with resource-index claims.

Each physical path maps to one owner:

- core game file: `game_files`
- resource baseline file: resource index
- launcher metadata: final metadata sync

Path ownership uses the resolved physical path. Links to an external asset root resolve to the owner of their concrete target.

If `game_files` and a resource index name the same physical path with different size or MD5 values, planning stops before file writes start.

Expired delivery URLs refresh before closure and metadata sync only when the live response matches version, canonical `file_path`, and encrypted `game_files` MD5. Saved manifest entries remain unchanged.

---

## 3. StreamingAssets Baseline

The launcher resource baseline uses full `index_*` file lists under `StreamingAssets`.

The resource plan owns selected payload files, `index_initial.json`, and `index_main.json`.

Payload files finish first, then encrypted index documents write atomically.

Griffr records baseline state in:

```text
.griffr/resource-baseline.json
.griffr/resource-baseline.pending.json
```

A pending file pins the resource version and index identity across resume.

Obsolete cleanup removes files only if they were recorded in the prior baseline, are absent from the new plan, and match their previous size and MD5. Modified files remain on disk.

---

## 4. Final Integrity Scope

Install and update run a final closure pass against the command-scoped `ContentPlan`. `ArtifactClaim` records preserve resource ownership even if tasks move into subgraphs.

Verification supports three scopes:

- `all`: core game files and launcher resource baseline
- `core`: core files only; direct alias for `verify --skip-vfs`
- `resources`: launcher resource baseline only

During repair, launcher metadata is excluded from generic file repair. `game_files` and `package_files` commit after payload closure; `config.ini` commits last.

---

## 5. Persistent Working Set

`resources sync` handles the game-selected working set under `Persistent`.

Persistent rules:

- file selection uses `pref_initial.json` and `pref_main.json` exclusively; full `index_*` files are never used as fallbacks;
- missing preference files return an error;
- candidate reuse copies bytes without hardlinks;
- downloaded preference manifests write atomically after payload completion;
- cleanup is off by default; `--prune` removes only unchanged files recorded in prior Persistent state;
- execution stops if the game process is running.

Persistent state uses:

```text
.griffr/persistent-vfs.json
.griffr/persistent-vfs.pending.json
```

---

## 6. Patch Asset Storage

The patch protocol field `vfs_base_path` keeps its upstream name. Project-facing storage uses the broader asset term.

`--external-asset-root` stores the patch-managed asset tree outside the install root and links the protocol path. Path details are saved in `.griffr-storage.json`. All operations (archive, patch, repair, proof, cleanup) resolve physical paths to keep single-source ownership.



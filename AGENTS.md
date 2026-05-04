# Agent Workflow

Still in active development.
ALWAYS introduce breaking changes for performance and maintainability.

## Scope
- Workspace crates:
  - `crates/griffr-common` (shared library)
  - `crates/griffr-cli` (CLI binary: `griffr`)
- Protocol references:
  - `docs/API.md`, `docs/API_CORE.md`, `docs/API_CONFIG.md`, `docs/API_PROTOCOL.md`, `docs/API_RESOURCES.md`, `docs/API_MEDIA.md`, `docs/API_LAUNCHER.md`
- Design references:
  - `docs/DESIGN_compio.md`, `docs/DESIGN_task_pool.md`
- Reverse-engineering references:
  - `ref/`

## Required Lifecycle
1. Write down all actionable items into `docs/TODO.md`.
2. Pick next actionable item from `docs/TODO.md`.
3. Implement minimal, focused changes.
4. Verify empirically (command runs and/or tests).
5. Update `docs/TODO.md` only after successful verification, including a short verification note.

## Verification Baseline
- Run relevant tests (`cargo test`, plus targeted crate/command checks as needed).
- Validate affected command flows (`install`, `update`, `verify`, `launch`) when touched.
- Prefer Windows/PowerShell-native validation paths.

## Mandatory Task-Pool / DAG Rules
1. Use one command-scoped `TaskPoolRunner` per invocation (`install`, `update`, `verify --repair`).
2. `download_vfs_resources` must use the caller's runner (no internal pool).
3. Run dependent integrity work as a single DAG batch; include VFS tasks via `extra_tasks`.
4. Avoid duplicated executor branches; conditionally build `extra_tasks`, then run once.
5. `verify --repair --relink-reuse` requires `--reuse-from`.
6. `verify` must expose `--skip-vfs` (parity with `install`/`update`).
7. Reuse policy:
   - `prefer_reuse = false` for normal install/update materialization and VFS sync.
   - `prefer_reuse = true` only for explicit relink mode.
8. Preserve correctness barriers:
   - archive/materialization completion before verification DAG
   - no verify race against unmateralized files unless represented in same DAG
9. New install/update/verify phases must integrate into shared runner + DAG model by default; extra pools require explicit code-comment justification.

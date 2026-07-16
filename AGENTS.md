# Griffr Agent Instructions

Griffr is in active prerelease development. Prefer a clean, maintainable design over backward compatibility. Breaking changes are expected when they remove duplication, improve correctness, or simplify the architecture.

Do not add migration, compatibility, or deprecation layers unless the user explicitly requests them. In prerelease code, obsolete models and APIs should normally be removed completely rather than preserved beside their replacements.

## Repository Scope

Primary workspace crates:

- `crates/griffr-common`: shared domain logic, task execution, protocols, storage, and frontend-neutral runtime APIs.
- `crates/griffr-cli`: command parsing, terminal presentation, and CLI-specific orchestration.
- Future GUI crates must consume shared APIs without requiring terminal-specific dependencies in `griffr-common`.

Reference material:

- API documentation: `docs/API*.md`
- Design documentation: `docs/DESIGN_*.md`
- Patch pipeline documentation: `docs/PATCH_PIPELINE.md`
- Reverse-engineering references: `ref/`
- Static checker: `scripts/rust_check.py` and `scripts/rust_check_lib/`

Always obey the closest nested `AGENTS.md` when working inside a subdirectory.

## Working Lifecycle

1. Record actionable work in `docs/TODO.md` when that file is part of the current workflow.
2. Select one coherent change set.
3. Inspect all producers, consumers, tests, and public re-exports before editing an API.
4. Implement the smallest complete architectural change, not a partial compatibility layer.
5. Run structural checks and relevant tests.
6. Fix findings rather than weakening checks merely to make the patch pass.
7. Update documentation and `docs/TODO.md` only after successful verification.
8. Package only after the working tree passes the available checks.
9. Re-extract the package and verify the extracted artifact again.

For large refactors, keep intermediate changes internally consistent. Do not leave both old callback APIs and new channel APIs active unless a short-lived adapter is strictly required to land the migration safely.

## Single Source of Truth

Every domain fact should have one authoritative representation.

- Do not introduce parallel structs that describe the same game, target, server, profile, catalog entry, channel, or installation identity without a demonstrated semantic difference.
- Derive views and serialization formats from the canonical model.
- Remove obsolete storage keys, serialized keys, configuration objects, and compatibility fields when no longer used.
- Do not copy canonical constants into CLI or GUI code. Re-export or consume them from their owning module.
- When two values look similar, determine whether they are genuinely different concepts before retaining separate types.

## Cross-Crate API Rules

Shared APIs must expose domain semantics, not frontend mechanics.

- `griffr-common` must not depend on `indicatif`, terminal styling, CLI message wording, or GUI toolkit types.
- CLI and future GUI crates own rendering state and presentation policy.
- Avoid exported callback-heavy APIs. In particular, do not expose progress through `Fn`, `FnMut`, or `FnOnce` parameters across crate boundaries.
- Do not expose raw `flume::Sender<ProgressUpdate>` or `flume::Receiver<ProgressUpdate>` types. Use the shared wrapper types.
- Implementation-local synchronous callbacks remain acceptable in crate-private helpers when they are tightly scoped and do not define a cross-crate contract.
- Prefer an explicit options/context struct when a public function accumulates many independent parameters.

## Task-Pool and DAG Rules

1. Use one command-scoped `TaskPoolRunner` per `install`, `update`, or `verify --repair` invocation.
2. `download_vfs_resources` must use the caller's runner; do not create a hidden internal pool.
3. Represent dependent work as one DAG batch where practical, including VFS work through `extra_tasks`.
4. Avoid duplicated executor branches. Build conditional task lists, then run the shared executor once.
5. `verify --repair --relink-reuse` requires `--reuse-from`.
6. `verify` must retain `--skip-vfs` parity with install and update flows.
7. Reuse policy:
   - normal install/update materialization and VFS sync use `prefer_reuse = false`;
   - explicit relink mode may use `prefer_reuse = true`.
8. Preserve correctness barriers:
   - archive/materialization completion before dependent verification;
   - no verification race against files that have not been materialized unless dependencies are represented in the same DAG.
9. New install/update/verify phases should integrate with the shared runner and DAG model by default. Additional pools require a code comment explaining why the shared runner cannot be used.
10. Preserve forward-only patch transaction barriers:
    - preflight the archive and persist the selected plan before staged files mutate the install;
    - defer `config.ini` and other completion markers until VFS materialization and cleanup succeed;
    - release a patch base only after its final consumer commits;
    - delete-only paths may be removed early, but planned outputs and still-referenced bases must remain protected.

## Static Analysis Policy

Repository-specific invariants belong in `scripts/rust_check_lib`, not in disposable grep scripts or one-off review commands.

When adding a checker rule:

- implement it in the appropriate reusable checker module;
- reason about effective Rust visibility and module reachability rather than matching every textual occurrence;
- attach a stable diagnostic code;
- include evidence and limitations in inferred diagnostics;
- add positive and negative regression fixtures under `scripts/tests`;
- document the rule in `scripts/rust_check_lib/README.md`;
- keep the rule focused on architecture or semantics that rustc and Clippy do not already enforce reliably.

Current progress rules are the `PRG***` family. Do not bypass them by renaming types or hiding equivalent callback/channel forms behind aliases.

The Python checker is deliberately high-recall and review-oriented. Prefer reducing false positives through better parsing, module analysis, name resolution, and evidence—not by removing useful checks.

The checker is not a compiler. It must not claim to replace rustc, rustfmt, Clippy, or tests.

## Verification Commands

Preferred full Rust validation when the toolchain is available:

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Run targeted tests while iterating, then run the workspace-level commands before packaging.

Python structural analysis with pinned parser versions:

```powershell
uv run --with tree-sitter==0.23.2 --with tree-sitter-rust==0.23.2 `
  scripts/rust_check.py . --run-tools never
```

Checker regression suite:

```powershell
uv run --with tree-sitter==0.23.2 --with tree-sitter-rust==0.23.2 `
  python -m unittest discover -s scripts/tests -v
```

Also run, when available:

```powershell
uv run ruff check scripts
uv run python -m compileall -q scripts
```

If Cargo or the Rust toolchain is unavailable, run all available Python checks and state clearly that compiler-level validation was not performed. Never present the structural checker as equivalent to a successful Cargo build.

## Functional Verification

Validate affected command flows when touched:

- `install`
- `update`
- `verify`
- `verify --repair`
- `predownload` and resume behavior
- VFS bootstrap/sync
- archive extraction, patch application, and delete manifests
- launch behavior when launch-related code changes

Prefer Windows and PowerShell-native tests because Windows is the primary platform and the runtime is designed around compio/IOCP.

For progress changes, test at least:

- independent lanes do not overwrite one another;
- each lane retains one unit;
- the first visible state is available before the first long item completes;
- retries do not move byte progress backward;
- no-repair and all-reuse paths terminate cleanly;
- receiver closure terminates the renderer;
- disabling progress has negligible behavioral impact;
- progress delivery failure does not fail the operation;
- durable result history contains no transient samples.

## Packaging

When the user requests a ZIP:

1. Remove build artifacts and transient caches that should not ship, especially Python `__pycache__` and `.pyc` files.
2. Preserve repository-relative paths.
3. Create the archive from the verified working tree.
4. Extract it into a fresh directory.
5. Run the structural checker and regression suite against the extracted copy.
6. Run Cargo checks against the extracted copy when the toolchain is available.
7. Report the archive SHA-256.

Do not claim a packaged artifact passed checks that were run only against a different working directory.

## Documentation Expectations

Update design documentation when an architectural contract changes.

In particular, keep the following aligned with code:

- task-pool event and outcome model;
- progress protocol and lane catalog;
- patch execution order;
- API/channel configuration;
- checker diagnostic documentation.

Documentation should describe the current design, not preserve a history of obsolete prerelease APIs.

## Review Priorities

When reviewing or modifying code, prioritize in this order:

1. correctness and filesystem safety;
2. single source of truth;
3. task dependency correctness;
4. frontend-neutral crate boundaries;
5. deterministic progress semantics;
6. cancellation, retry, and error behavior;
7. maintainability and testability;
8. performance on the Windows large-file I/O workload;
9. user-facing polish.

Do not trade correctness for a smoother-looking progress bar. Progress is an observation of work, never the source of truth for work completion.

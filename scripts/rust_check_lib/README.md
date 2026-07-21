## Python Rust Static Analysis

`scripts/rust_check.py` is an aggressive, tree-sitter-based review pass. It is
not a replacement for rustc, rustfmt, Clippy, or tests. When Cargo is available,
those tools remain authoritative; the Python pass provides a useful fallback
and enforces repository-specific architecture rules that Cargo does not know.

Run the Python-only analysis with the pinned parser versions:

```bash
uv run --with tree-sitter==0.23.2 --with tree-sitter-rust==0.23.2 \
  scripts/rust_check.py . --run-tools never
```

Apply conservative fixes and then re-run analysis on the changed tree:

```bash
uv run --with tree-sitter==0.23.2 --with tree-sitter-rust==0.23.2 \
  scripts/rust_check.py . --run-tools never --fix
```

`--fix` is intentionally narrower than the diagnostics. Edits are collected as
byte ranges, conflicting or overlapping edits are skipped, changed files are
reparsed, and analysis is repeated until no further safe edit is available.
Running `--fix` a second time should apply zero edits.

The default policy favors recall:

- all `definite`, `probable`, and `speculative` diagnostics are shown;
- warnings make the command exit non-zero (`--fail-on warning`);
- every inferred diagnostic includes confidence, evidence, and limitations;
- cfg compatibility, module reachability, imports/re-exports, selected macro
  output, lexical scopes, and direct-call arity are analyzed across files;
- repository architecture checks enforce frontend-neutral progress channels, canonical
  progress lanes, durable-only task-pool results, and a Dispatcher-only task execution model
  (no class-specific `std::thread`/`Condvar` worker pools or synchronous dispatch bridge);
- `DAG001` checks exhaustive `Task` routing matches that deliberately omit a catch-all, while
  `DAG002` keeps struct-like `Task::Variant { ... }` constructors synchronized with the canonical
  enum payload. These are high-confidence structural fallbacks for large DAG refactors, not a
  replacement for rustc type checking;
- `AFS001` rejects direct `std::fs` calls and blocking `Path` probes in production async functions
  and async blocks, while recognizing `spawn_blocking`, `dispatch_blocking`, and `run_blocking`
  closures as explicit synchronous boundaries;
- `AFS002` reports blocking closures that contain only operations with compio async replacements;
- `AFS003` follows direct calls into local synchronous helpers and rejects filesystem work hidden
  behind those helpers when it is invoked from production async code.
- `WRD001` rejects abstract project wording listed in `docs/WORDING.md`, and `WRD002` rejects broad file names when a concrete name is available.
  Test fixture modules are excluded because synchronous fixture construction is not a runtime I/O
  architecture decision. Directory enumeration, recursive removal, link reads, and canonicalization
  remain valid blocking boundaries with compio 0.19.

Useful policy overrides:

```bash
# Review everything without failing the command.
uv run scripts/rust_check.py . --run-tools never --fail-on never

# Keep only diagnostics at least as strong as probable.
uv run scripts/rust_check.py . --min-confidence probable

# Treat the Python pass as a no-Cargo gate while excluding speculative findings.
uv run scripts/rust_check.py . --run-tools never \
  --min-confidence probable --fail-on warning

# Delegate to Cargo tools and fail when they are unavailable.
uv run scripts/rust_check.py . --run-tools required --cargo-test
```

Run the checker regression suite:

```bash
uv run --with tree-sitter==0.23.2 --with tree-sitter-rust==0.23.2 \
  python -m unittest discover -s scripts/tests -v
```

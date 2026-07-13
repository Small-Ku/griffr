## Python Rust Static Analysis

`scripts/rust_check.py` is an aggressive, tree-sitter-based review pass. It is
not a replacement for rustc, rustfmt, or Clippy; when Cargo is available it can
run those tools as authoritative checks as well.

Run the Python-only analysis with pinned parser versions:

```bash
uv run --with tree-sitter==0.23.2 --with tree-sitter-rust==0.23.2 \
  scripts/rust_check.py . --run-tools never
```

The default policy favors recall:

- all `definite`, `probable`, and `speculative` diagnostics are shown;
- warnings make the command exit non-zero (`--fail-on warning`);
- every inferred diagnostic includes confidence, evidence, and limitations;
- cfg compatibility, module reachability, imports/re-exports, selected macro
  output, lexical scopes, and direct-call arity are analyzed across files;
- repository architecture checks enforce frontend-neutral progress channels, canonical
  progress lanes, and durable-only task-pool results.

Useful policy overrides:

```bash
# Review everything without failing the command.
uv run scripts/rust_check.py . --run-tools never --fail-on never

# Keep only diagnostics at least as strong as probable.
uv run scripts/rust_check.py . --min-confidence probable

# Delegate to Cargo tools and fail when they are unavailable.
uv run scripts/rust_check.py . --run-tools required --cargo-test
```

Run the checker regression suite:

```bash
uv run --with tree-sitter==0.23.2 --with tree-sitter-rust==0.23.2 \
  python -m unittest discover -s scripts/tests -v
```
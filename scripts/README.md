# Repository checks

Run the dependency-free repository policy checker from the repository root:

```bash
python scripts/check_repo.py .
```

Run its regression suite with:

```bash
python -m unittest discover -s scripts/tests -v
```

The checker intentionally covers only policies that Rust's normal tools do not
understand:

| Code | Policy |
| --- | --- |
| `ARC001` | `griffr-common` stays independent from terminal and GUI renderer crates. |
| `PRG001` | Raw `ProgressUpdate` channels stay private to the canonical wrapper module. |
| `PRG002` | Progress lanes come from the shared lane catalog. |
| `PRG003` | Public shared APIs do not expose progress callbacks. |
| `DSP001` | The task pool uses Dispatcher and admission limits, not custom worker pools. |
| `AFS001` | Production async code does not call explicit blocking `std::fs` APIs outside a blocking boundary. |
| `SSOT001` | Removed duplicate model names do not return. |
| `NAM001` | Source files use concrete names instead of broad container names. |
| `REP001` | A repository policy input cannot be read or parsed. |

The checker does not run Cargo and does not implement Rust formatting, syntax,
name resolution, module loading, cfg evaluation, unused-code detection, or
Clippy-style lints. Use these commands for those concerns:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

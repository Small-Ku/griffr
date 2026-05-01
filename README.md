# griffr

Rust workspace for a Hypergryph game launcher/downloader CLI.

## Workspace
- `crates/griffr-common`: shared library crate
- `crates/griffr-cli`: CLI crate (binary: `griffr`)

## Prerequisites
- Rust toolchain (`edition = "2021"`, `rust-version = "1.78"`)

## Common Commands

Build:
```bash
cargo build
```

Run CLI help:
```bash
cargo run -p griffr-cli -- --help
```

Run subcommand help:
```bash
cargo run -p griffr-cli -- <SUBCOMMAND> --help
```

Tests:
```bash
cargo test
cargo test -p griffr-common
cargo test -p griffr-cli
```

Format/Lint:
```bash
cargo fmt --all
cargo clippy --all-targets --all-features
```

Release build:
```bash
cargo build --release
```

## Documentation
- API/protocol docs: `docs/API.md`
- Design docs: `docs/DESIGN_*.md`

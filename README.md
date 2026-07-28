# griffr

Rust workspace for a Hypergryph game launcher/downloader CLI.

## Workspace
- `crates/griffr-common`: shared library crate
- `crates/griffr-cli`: CLI crate (binary: `griffr`)

## Prerequisites
- Rust toolchain

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


## Resource Commands

Install using launcher resource index:
```bash
cargo run -p griffr-cli -- install --game endfield --region sg --path <GAME_PATH> --resource-policy auto
```

Update using packages and `game_files` only (`--skip-vfs` is an alias for `--resource-policy package-only`):
```bash
cargo run -p griffr-cli -- update --path <GAME_PATH> --resource-policy package-only
```

Verify core files only:
```bash
cargo run -p griffr-cli -- verify --path <GAME_PATH> --scope core
```

Set up game-selected Persistent working set (copy-only; add `--prune` to clean unchanged recorded baseline):
```bash
cargo run -p griffr-cli -- setup-persistent-resources --path <GAME_PATH>
```

## Documentation
- API/protocol docs: [`docs/API.md`](docs/API.md)
- Design & architecture docs: [`docs/DESIGN.md`](docs/DESIGN.md)
- Resource ownership and scope: [`docs/DESIGN_resources.md`](docs/DESIGN_resources.md)

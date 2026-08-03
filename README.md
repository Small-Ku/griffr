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
python scripts/check_repo.py .
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

## Wine on Linux

On non-Windows hosts, `launch` runs the game through Wine. It uses `wine` by
default and inherits `WINEPREFIX` when set:

```bash
WINEPREFIX="$HOME/.wine-endfield" cargo run -p griffr-cli -- launch --path <GAME_PATH>
```

An explicit Wine-compatible runner and prefix can be selected when several
Wine environments are installed:

```bash
cargo run -p griffr-cli -- launch --path <GAME_PATH> \
  --wine /opt/wine/bin/wine64 \
  --wine-prefix "$HOME/.wine-endfield"
```

`--force` only signals Linux processes that Griffr can prove belong to the
selected install by their process name and `/proc` path information. The
runner can also be set through `GRIFFR_WINE`.

The deterministic Linux integration test uses a Wine-compatible shim and a
real child process. A real Wine smoke test generates a small PE executable and
can be run on a host with Wine, clang, and lld-link:

```bash
cargo test -p griffr-common real_wine_launch_smoke -- --ignored --nocapture
```

## Documentation
- API/protocol docs: [`docs/API.md`](docs/API.md)
- Design & architecture docs: [`docs/DESIGN.md`](docs/DESIGN.md)
- Resource ownership and scope: [`docs/DESIGN_resources.md`](docs/DESIGN_resources.md)

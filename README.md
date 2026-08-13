# griffr

Rust workspace for a Hypergryph, Gryphline, and YoStar game launcher/downloader CLI.

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
cargo run -p griffr-cli -- install --game endfield --region sg --path <GAME_PATH> --resources auto
```

Update using packages and `game_files` only:
```bash
cargo run -p griffr-cli -- update --path <GAME_PATH> --resources package-only
```

Verify core files only:
```bash
cargo run -p griffr-cli -- verify --path <GAME_PATH> --scope core
```

### YoStar Arknights EN

Arknights EN uses the YoStar launcher protocol and native launcher metadata. It does not take Hypergryph channel/sub-channel arguments:

```bash
cargo run -p griffr-cli -- install --game arknights --region en --path <GAME_PATH>
cargo run -p griffr-cli -- update --path <GAME_PATH>
cargo run -p griffr-cli -- verify --path <GAME_PATH> --scope core
cargo run -p griffr-cli -- verify --path <GAME_PATH> --scope core --repair
cargo run -p griffr-cli -- launch --path <GAME_PATH>
```

The YoStar backend currently supports install, update, offline full verify, repair, launch, info, and uninstall. Normal updates trust unchanged manifest entries after existence/size checks, while full `verify` computes CRC64-XZ. YoStar has no observed equivalent of the Hypergryph predownload or Persistent/VFS resource-index APIs, so `stage`/predownload and resource synchronization are not available for this backend.

Synchronize the game-selected Persistent working set (copy-only; add `--prune` to clean unchanged recorded baseline):
```bash
cargo run -p griffr-cli -- resources sync --path <GAME_PATH>
```

Stage update archives without applying them, then require that exact stage during update:
```bash
cargo run -p griffr-cli -- stage fetch --path <GAME_PATH> --stage-dir <STAGE_DIR>
cargo run -p griffr-cli -- update --path <GAME_PATH> --stage-dir <STAGE_DIR> --require-staged
```

Resume an interrupted patch transaction:
```bash
cargo run -p griffr-cli -- recover --path <GAME_PATH>
```

Batch update or verify defaults to keep-going. Use `--fail-fast` for a serial batch, or `--jobs N` for targets on independent storage volumes.

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

Wine/process behavior is tested separately from the cross-platform content
lifecycle. A real Wine smoke test generates a small PE executable and can be
run on a host with Wine, clang, and lld-link:

```bash
cargo test -p griffr-common real_wine_launch_smoke -- --ignored --nocapture
```

## Documentation
- API/protocol docs: [`docs/API.md`](docs/API.md)
- Design & architecture docs: [`docs/DESIGN.md`](docs/DESIGN.md)
- Resource ownership and scope: [`docs/DESIGN_resources.md`](docs/DESIGN_resources.md)
- Deterministic and official-server testing: [`docs/TESTING.md`](docs/TESTING.md)

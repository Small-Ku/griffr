# Testing

## Deterministic platform lifecycle

`crates/griffr-cli/tests/cli_e2e.rs` starts deterministic loopback launcher/CDN services and
executes the built `griffr` process. The fixture covers both Hypergryph/Gryphline and
YoStar protocol backends without depending on production servers. The same content-management
lifecycle runs on Linux and Windows; real game process launch remains outside this test.

```bash
cargo test -p griffr-cli --test cli_e2e --locked -- --nocapture
```

The lifecycle covers install, local/remote info, verify and repair, explicit
same-filesystem reuse, hardlink-safe staged update, reuse-assisted update,
concurrent multi-path verify, account capture/activate, remote debug/media
calls, Persistent resource sync, snapshots, detach, and uninstall.

The YoStar Arknights KR/EN/JP fixture separately exercises each regional target,
dry-run and real install, native `manifest.json` / `game-launcher-config.json` metadata,
CRC64-XZ full verify and
repair, primary/backup CDN failover, manifest-driven normal update, `.griffr/state.json`
launch barriers, launch-time existence/size quick checks, unsupported predownload
contracts, and uninstall. It deliberately verifies the YoStar distinction between a
wrong-size corruption (blocked by launch preflight) and same-size corruption (detected
only by full verify).

Hardlink assertions are physical identity checks rather than content checks:

- Linux and other Unix hosts compare filesystem device, inode, and link count.
- Windows compares volume serial number, file index, and link count through
  `GetFileInformationByHandle` via the existing `windows-sys` dependency. This
  keeps the E2E suite compatible with the repository's Rust 1.97.1 stable CI.

`.github/workflows/platform-e2e.yml` runs this suite on Ubuntu 24.04 and Windows
Server 2025 with Rust 1.97.1. The equivalent local Windows entry point is:

```powershell
scripts/test_windows_e2e.ps1
```

## Official-server content lifecycle

`crates/griffr-cli/tests/live_cli_e2e.rs` is ignored by default because it
performs a full current-version download and deletes its own test installs. It
runs the native Linux or Windows `griffr` executable; Wine and game launch are
not involved.

Linux example:

```bash
scripts/test_live_cli_e2e.sh /mnt/test-volume/griffr-live endfield cn 1
```

Windows example:

```powershell
scripts/test_live_cli_e2e.ps1 `
  -Root G:\griffr-live-e2e `
  -Game endfield `
  -Region cn `
  -Channel 1
```

The root must be a dedicated directory on the filesystem being tested. A unique
`run-*` child is created; a failed run is retained for diagnosis, while a
successful run exercises detach and uninstall and removes the empty child.

The official-server lifecycle performs:

1. Production news, latest-game, latest-resources, and game-files requests.
2. Current full install with package-only resource policy.
3. Core verification and remote/local metadata inspection.
4. A second install materialized from the first through explicit hardlink reuse.
5. Physical hardlink identity and link-count verification.
6. Removal of one linked path followed by CDN repair, proving the peer remains
   intact and the repaired path receives a distinct physical file.
7. Explicit relink, batched current-version update, and concurrent verification.
8. Persistent resource sync (`base` by default) and all-scope verification.
9. Predownload inspection, recover negative contract, detach, and uninstall.

Set `GRIFFR_LIVE_E2E_RESOURCES=off` to omit Persistent resources or `all` to
synchronize both resource groups. `GRIFFR_LIVE_E2E_FETCH_PREDOWNLOAD=1` downloads
an advertised predownload payload but does not apply a future release.

A current installation cannot prove a real version transition. To test an
actual official patch/full update, pass an explicitly disposable old install:

```powershell
scripts/test_live_cli_e2e.ps1 `
  -Root G:\griffr-live-e2e `
  -Game endfield -Region cn -Channel 1 `
  -DisposableUpdatePath G:\griffr-disposable-old-install
```

The equivalent Linux invocation is:

```bash
scripts/test_live_cli_e2e.sh \
  /mnt/test-volume/griffr-live endfield cn 1 "" base \
  /mnt/test-volume/griffr-disposable-old-install
```

That path is updated and verified but is not deleted by the harness. Without
such a seed, the update stage is correctly reported as a current-version/no-op
path rather than claimed as a delta-update test.

## Bounded streaming payload soak

If the test volume cannot retain a complete installation, use the separate
streaming harness:

```bash
scripts/test_live_streaming.sh /mnt/test-volume/griffr-stream endfield cn 1
```

The harness fetches the current full-package metadata, then processes one
payload at a time through the same bounded HTTP-body writer used by the task
pool. Each payload must pass its declared size and MD5 checks and its atomic
commit before the harness removes it. The report includes per-payload and
aggregate wall-clock throughput, while the temporary root is kept bounded by
one active payload.

This mode deliberately does not claim a full install lifecycle: it does not
retain an install tree and therefore cannot prove final-tree verify, repair,
reuse, or hardlink behavior. Use `test_live_cli_e2e.sh` when those assertions
are required.

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

`CI` compiles this suite once per platform with the `ci` Cargo profile, archives the
result with cargo-nextest, and fans the archive out across four deterministic hash shards on
Ubuntu 24.04 and Windows Server 2025. The test runners do not rebuild the workspace. The
equivalent local Windows entry point is:

```powershell
scripts/test_windows_e2e.ps1
```


## GitHub Actions test topology

The normal `.github/workflows/ci.yml` workflow is the pull-request and main-branch
authority. It separates cheap policy checks from expensive compilation and then reuses the
compiled test payload:

1. `repository-policy`, `rustfmt`, and feature-boundary jobs fail early without waiting for
   the full workspace test build. `actionlint` validates every workflow in the same policy
   lane.
2. Linux and Windows each compile the workspace test graph once with `[profile.ci]` and
   `cargo nextest archive`. The archive contains test executables, required dynamic libraries,
   Cargo metadata, and non-test binaries needed by integration tests.
3. Four `hash:m/4` nextest jobs per platform download that archive and run independent
   deterministic shards. The real-process CLI E2E is part of this archive.
4. Platform `cargo check`, release-profile check, all-features check, and `clippy -D warnings`
   fan out in parallel with the archive builds after the cheap repository-policy/rustfmt gates.
   They still reuse sccache entries from prior jobs/runs when compiler flags match, without putting
   the archive build on the critical path. Linux additionally runs doctests and builds rustdoc with
   warnings denied, which nextest does not cover.

Cargo registry/index and Git dependency source downloads are cached separately by OS, architecture,
and `Cargo.lock`. `sccache` uses the GitHub Actions backend for compiler objects across jobs and
workflow runs; workflow artifacts are used only to transport the already-linked nextest test archive
within a single run. The repository deliberately does not cache or upload the whole Cargo `target/`
directory, so incremental compiler state never becomes a cross-run correctness dependency.

`.github/workflows/nightly.yml` is a weekly compatibility lane on Linux and Windows. It runs
check, clippy, and the non-ignored workspace tests with current Rust nightly. Stable Rust
1.97.1 remains the required/gating compiler in normal CI.

For branch protection, require the single **`CI / Required`** check rather than every matrix
expansion. That aggregate job runs with `always()` and fails unless repository policy, rustfmt,
feature boundaries, both platform build archives, both quality jobs, and both platform test matrices
all finished successfully. This keeps the protected check name stable if shard counts or internal job
names change.

`.github/workflows/extended-platform.yml` is a separate weekly/manual host-integration lane.
Its Ubuntu job installs Wine, clang, and lld, builds a tiny generated Windows PE fixture, launches
it through Griffr's real Wine launcher, observes the process, then stops it. Keeping this outside
normal CI tests the host integration without making every pull request install Wine.

### Production/live test policy

Production services are intentionally not part of pull-request correctness. The live workflow
has four lanes with different risk:

- **smoke** is read-only. During a trusted push to `main`, `CI` evaluates the exact
  `before..sha` push range with `scripts/ci/live_e2e_policy.py` and publishes that decision as
  a short-lived workflow artifact. After the same CI run succeeds, `Live E2E` consumes that
  artifact instead of guessing a single-commit diff. Provider/core/CLI changes that can
  affect remote protocol behavior schedule smoke. If a manually re-run/old CI run has already lost
  its tiny policy artifact, the post-CI workflow falls back conservatively to smoke only rather than
  guessing destructive coverage. A daily scheduled smoke also detects API drift even when the
  repository has not changed. It covers CLI requests for Endfield CN/global
  and YoStar Arknights EN/JP/KR, plus the current Hypergryph latest/media/game-files channel
  matrix. No install tree is created.
- **archive-sample** performs bounded range reads against the current official multi-volume
  Endfield archive and validates the split/archive format assumptions used by the extractor. It
  is manual because it contacts the production CDN and may cache hundreds of MiB, even though
  it does not retain a game installation.
- **lifecycle** performs the full retained install/verify/repair/reuse/update/detach/uninstall
  lifecycle and can download a complete game. It is never started automatically.
- **streaming** downloads official package parts through the production bounded writer, checks
  size/hash/atomic commit, and discards each part. It can transfer the complete package set even
  though peak disk use is bounded, so it is also manual only.

`archive-sample`, `lifecycle`, and `streaming` run only through `workflow_dispatch` on a dedicated
self-hosted runner labelled `griffr-live` plus `linux` or `windows`. A manual run always performs
the hosted read-only smoke first; the selected large/destructive lane is released only after that
smoke passes, and then references the `live-e2e` GitHub Environment. Configure that environment in repository Settings with required
reviewers; enabling **Prevent self-review** is recommended for deletion-capable/large-download
runs. Keep those self-hosted runners at Actions Runner v2.327.1 or newer because the workflow uses
Node 24-generation actions. The supplied `root` must point at a dedicated test filesystem root.

The path policy is advisory for dangerous lanes: extractor changes may recommend
`archive-sample`; runtime install changes may recommend `lifecycle`; and downloader/task-pool
changes may additionally recommend `streaming`. Automatic execution is still capped at
read-only smoke. This keeps production outages, large downloads, deletion-capable tests, and
untrusted pull-request code out of the required PR gate while still showing reviewers which
live lane should be run before a risky release.

Historical-version probes remain ignored/manual. Their purpose is diagnosing a known old
launcher response or confirming patch/full behavior for a recorded version pair; deterministic
unit/E2E tests own the stable package-selection contract, so a provider deleting old metadata
should not make normal CI red.

To inspect the recommendation locally:

```bash
python scripts/ci/live_e2e_policy.py --base origin/main --head HEAD
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

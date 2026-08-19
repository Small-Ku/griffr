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
| `ARC001` | Core, provider API, and runtime crates stay independent from terminal and GUI renderer crates. |
| `ARC002` | `griffr-core` stays below network/runtime/platform layers, and provider API crates never depend on runtime or each other. |
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


## CI/live-test policy helper

Classify a change without contacting production services:

```bash
python scripts/ci/live_e2e_policy.py --base origin/main --head HEAD
```

The output recommends `smoke`, `archive-sample`, `lifecycle`, and/or `streaming`. `smoke` is
read-only and may run automatically after main-branch CI succeeds; the other lanes are advisory
until a maintainer explicitly dispatches the `Live E2E` workflow. Smoke fans out by deployment.
Manual large lanes accept `runner_os=all|linux|windows` and `target=all|<known deployment>`; the
`full-matrix` mode releases archive sampling, lifecycle, and streaming as independent job families
after smoke. Each platform compiles one relocatable workspace nextest archive and every target cell
on that platform reuses it. Post-CI smoke reuses the exact Linux archive from the triggering CI run
when available. Large jobs remain protected by the `live-e2e` GitHub Environment and use
`scripts/ci/prepare_live_workspace.py` to select a safe `RUNNER_TEMP` root and enforce a free-space
floor before production payload I/O. The retained lifecycle adds a package-aware manifest-footprint
preflight; if a full game cannot fit, use the bounded streaming lane or the local lifecycle harness
on a roomier volume instead of maintaining a default self-hosted Actions runner.
The classifier has regression coverage in `scripts/tests/test_live_e2e_policy.py`.
`test_ci_workflows.py` additionally locks the intended build-once/shard topology, the aggregate
required gate, sccache setup, hosted-first live execution, build-space reclamation, and smoke/disk
gates for destructive lanes.

Normal CI compiles a cargo-nextest archive once per operating system with Cargo `profile.ci` and
runs four hash-partitioned shards from the archive. Cargo registry/Git source downloads are cached
separately; `sccache` caches compiler outputs. Neither is a replacement for the archive, and the
workflow does not persist the complete `target/` tree.

## Stable and nightly Linux toolchain matrix

The repository defaults to the stable LLVM backend so every Cargo command works
with the minimum supported stable toolchain. For standalone toolchains unpacked
from the official Linux tarballs, run the complete stable suite and the
nightly/Cranelift CLI E2E suite with:

```bash
scripts/test_linux_toolchains.sh /path/to/stable /path/to/nightly
```

The script uses separate target directories, which prevents Cargo build-lock
contention between LLVM and Cranelift artifacts. Set `GRIFFR_STABLE_TARGET_DIR`
or `GRIFFR_NIGHTLY_TARGET_DIR` to override them.

With a rustup-managed nightly, the corresponding manual check is:

```powershell
cargo +nightly -Zcodegen-backend \
  --config 'profile.dev.codegen-backend="cranelift"' \
  --config 'profile.dev.package.md5-many.codegen-backend="llvm"' \
  --config 'profile.dev.package.crc-fast.codegen-backend="llvm"' \
  --config 'profile.dev.package.griffr-runtime.codegen-backend="llvm"' \
  check --workspace --all-targets
```

The SIMD hashing path is the exception to the Cranelift backend in this matrix.
The pinned nightly Cranelift backend does not yet lower all AVX-512/PCLMUL
intrinsics used by `md5-many` and `crc-fast`; cross-crate inlining can also move
those operations into `griffr-runtime`. The script therefore compiles those two
dependencies and `griffr-runtime` with LLVM while the rest of the nightly
workspace remains on Cranelift. This avoids silently replacing unsupported SIMD
intrinsics with traps in the nightly E2E binary.

Do not commit an unconditional `profile.dev.codegen-backend` setting to
`.cargo/config.toml`: stable Cargo rejects that configuration before it can run
any command.

## Deterministic CLI end-to-end test

Run the real-process CLI lifecycle test with:

```bash
cargo test -p griffr-cli --test cli_e2e -- --nocapture
```

The test starts a loopback launcher/CDN service, creates encrypted `config.ini`,
`game_files`, resource indexes, and real ZIP/patch packages, then invokes the
built `griffr` executable as a child process. It covers every command's help
contract plus install, verify/repair, same-volume hardlink reuse, hardlink-safe
updates, two-target concurrent verify, account capture/activate, remote debug
and news calls, predownload inspect/fetch/apply, recover/resume error contracts,
Persistent VFS sync and snapshot/diff, detach, and uninstall. Linux checks
filesystem device/inode/link count; Windows checks volume serial/file index/link
count. Game launch is tested separately. The deterministic lifecycle does not
contact production services.

Run the same suite on a local Windows host with:

```powershell
scripts/test_windows_e2e.ps1
```

The ignored official-server lifecycle is available on both native platforms:

```bash
scripts/test_live_cli_e2e.sh /mnt/test-volume/griffr-live endfield cn 1
```

```powershell
scripts/test_live_cli_e2e.ps1 -Root G:\griffr-live-e2e -Game endfield -Region cn -Channel 1
```

When disk capacity cannot hold a complete game tree, run the bounded package
streaming soak instead:

```bash
scripts/test_live_streaming.sh /mnt/test-volume/griffr-stream endfield cn 1
```

Each official package part is downloaded through Griffr's production bounded
writer, size/MD5 verified, atomically committed, and immediately discarded.
This validates CDN transfer and integrity only; it is not a retained install,
repair, reuse, or hardlink test.

See `docs/TESTING.md` for deletion safeguards, resource modes, and the optional
disposable old-install input required to prove a real version transition.

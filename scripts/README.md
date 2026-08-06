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
cargo +nightly -Zcodegen-backend --config 'profile.dev.codegen-backend="cranelift"' check --workspace --all-targets
```

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
updates, two-target concurrent verify, Wine launch, account capture/activate,
remote debug and news calls, predownload inspect/fetch/apply, recover/resume
error contracts, Persistent VFS sync and snapshot/diff, detach, and uninstall.
It does not contact production services.

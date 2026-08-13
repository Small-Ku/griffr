#![feature(windows_by_handle)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use griffr_common::api::client::ApiClient;
use griffr_common::api::crypto::decrypt_game_files_owned;
use griffr_common::api::types::GameFileEntry;
use griffr_common::config::{resolve_api_target, ChannelPair, GameId, RegionId};
use griffr_common::runtime::task_pool::{download_and_discard, DEFAULT_PROGRESS_BUFFER_BYTES};
use md5::{Digest, Md5};

mod support;

use support::{assert_distinct_files, assert_same_hardlink};

const CONFIRMATION: &str = "I_ACCEPT_LARGE_DOWNLOADS_AND_TEST_DELETION";

#[derive(Debug)]
struct LiveConfig {
    run_root: PathBuf,
    primary: PathBuf,
    reuse: PathBuf,
    game: String,
    region: String,
    channel: Option<String>,
    sub_channel: Option<String>,
    resource_set: Option<String>,
    fetch_predownload: bool,
    disposable_update_path: Option<PathBuf>,
}

impl LiveConfig {
    fn from_env() -> Self {
        assert_eq!(
            env::var("GRIFFR_LIVE_E2E_CONFIRM").as_deref(),
            Ok(CONFIRMATION),
            "set GRIFFR_LIVE_E2E_CONFIRM={CONFIRMATION} to acknowledge the download and deletion scope"
        );
        let root = env::var_os("GRIFFR_LIVE_E2E_ROOT")
            .map(PathBuf::from)
            .expect("GRIFFR_LIVE_E2E_ROOT must name a dedicated filesystem directory");
        let root = prepare_safe_root(&root);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_secs();
        let run_root = root.join(format!("run-{}-{timestamp}", std::process::id()));
        fs::create_dir(&run_root).expect("create dedicated live E2E run directory");

        let resource_set = match env::var("GRIFFR_LIVE_E2E_RESOURCES") {
            Ok(value) if value.eq_ignore_ascii_case("off") => None,
            Ok(value) if matches!(value.as_str(), "base" | "all") => Some(value),
            Ok(value) => {
                panic!("GRIFFR_LIVE_E2E_RESOURCES must be off, base, or all; got {value:?}")
            }
            Err(env::VarError::NotPresent) => Some("base".to_string()),
            Err(error) => panic!("invalid GRIFFR_LIVE_E2E_RESOURCES: {error}"),
        };

        Self {
            primary: run_root.join("primary"),
            reuse: run_root.join("reuse"),
            run_root,
            game: required_env("GRIFFR_LIVE_E2E_GAME"),
            region: required_env("GRIFFR_LIVE_E2E_REGION"),
            channel: env::var("GRIFFR_LIVE_E2E_CHANNEL").ok(),
            sub_channel: env::var("GRIFFR_LIVE_E2E_SUB_CHANNEL").ok(),
            resource_set,
            fetch_predownload: env_flag("GRIFFR_LIVE_E2E_FETCH_PREDOWNLOAD"),
            disposable_update_path: env::var_os("GRIFFR_LIVE_E2E_UPDATE_PATH").map(PathBuf::from),
        }
    }

    fn target_args(&self) -> Vec<OsString> {
        let mut args = vec![
            "--game".into(),
            self.game.as_str().into(),
            "--region".into(),
            self.region.as_str().into(),
        ];
        if let Some(channel) = self.channel.as_deref() {
            args.extend(["--channel".into(), channel.into()]);
        }
        if let Some(sub_channel) = self.sub_channel.as_deref() {
            args.extend(["--sub-channel".into(), sub_channel.into()]);
        }
        args
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set explicitly"))
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn prepare_safe_root(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap_or_else(|error| {
        panic!("failed to create live E2E root {}: {error}", path.display())
    });
    let root = fs::canonicalize(path).unwrap_or_else(|error| {
        panic!(
            "failed to canonicalize live E2E root {}: {error}",
            path.display()
        )
    });
    assert!(
        root.components().count() > 1,
        "refusing filesystem root as GRIFFR_LIVE_E2E_ROOT: {}",
        root.display()
    );
    if let Some(home) = dirs::home_dir().and_then(|value| fs::canonicalize(value).ok()) {
        assert_ne!(root, home, "refusing home directory as live E2E root");
    }
    if let Ok(cwd) = env::current_dir().and_then(fs::canonicalize) {
        assert!(
            root != cwd && !cwd.starts_with(&root),
            "live E2E root must not contain the current repository: {}",
            root.display()
        );
    }
    root
}

fn run_command<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect::<Vec<_>>();
    println!(
        "\n$ griffr {}",
        args.iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    Command::new(env!("CARGO_BIN_EXE_griffr"))
        .args(&args)
        .env("NO_COLOR", "1")
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("run griffr")
}

fn command<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_command(args);
    if !output.status.success() {
        panic!(
            "griffr failed ({}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !output.stdout.is_empty() {
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }
    output
}

fn command_fails_with<I, S>(args: I, expected: &str)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_command(args);
    assert!(!output.status.success(), "command unexpectedly succeeded");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains(expected),
        "expected failure containing {expected:?}, got:\n{combined}"
    );
}

fn push_path(args: &mut Vec<OsString>, flag: &str, path: &Path) {
    args.push(flag.into());
    args.push(path.as_os_str().to_owned());
}

fn install_target(config: &LiveConfig) {
    let mut args = vec!["--quiet".into(), "install".into()];
    args.extend(config.target_args());
    args.extend(["--resources".into(), "package-only".into()]);
    push_path(&mut args, "--path", &config.primary);
    command(args);
}

fn parse_manifest(install: &Path) -> Vec<GameFileEntry> {
    let encrypted = fs::read(install.join("game_files")).expect("read installed game_files");
    let plaintext = decrypt_game_files_owned(encrypted).expect("decrypt installed game_files");
    plaintext
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("parse installed game_files entry"))
        .collect()
}

fn logical_path(root: &Path, logical: &str) -> PathBuf {
    let mut output = root.to_path_buf();
    for part in logical.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        assert_ne!(part, "..", "manifest path escapes install root: {logical}");
        let component = Path::new(part).components().next();
        assert!(
            matches!(component, Some(Component::Normal(_))),
            "invalid manifest component {part:?} in {logical:?}"
        );
        output.push(part);
    }
    output
}

fn choose_probe_entry(install: &Path, entries: &[GameFileEntry]) -> GameFileEntry {
    let mut candidates = entries
        .iter()
        .filter(|entry| entry.size > 0 && entry.size <= 64 * 1024 * 1024)
        .filter(|entry| {
            let normalized = entry.path.replace('\\', "/").to_ascii_lowercase();
            !matches!(
                normalized.as_str(),
                "config.ini" | "game_files" | "package_files"
            ) && !normalized.starts_with(".griffr/")
        })
        .filter(|entry| logical_path(install, &entry.path).is_file())
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by_key(|entry| entry.size);
    candidates.into_iter().next().unwrap_or_else(|| {
        panic!("no installed manifest file up to 64 MiB is available for repair probing")
    })
}

fn assert_md5(path: &Path, expected: &str) {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let actual = griffr_common::to_hex(&Md5::digest(bytes));
    assert_eq!(
        actual,
        expected.to_ascii_lowercase(),
        "MD5 mismatch for {}",
        path.display()
    );
}

fn copy_launcher_metadata(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("create reuse install root");
    for name in ["config.ini", "game_files"] {
        fs::copy(source.join(name), destination.join(name))
            .unwrap_or_else(|error| panic!("copy {name} into reuse install: {error}"));
    }
}

fn remote_smoke(config: &LiveConfig) {
    let mut news = vec!["news".into()];
    news.extend(config.target_args());
    command(news);

    for action in [
        "get-raw-latest-game",
        "get-raw-latest-resources",
        "list-game-files",
    ] {
        let mut args = vec!["debug".into(), action.into()];
        args.extend(config.target_args());
        command(args);
    }
}

fn verify_core(path: &Path) {
    let mut args = vec![
        "--quiet".into(),
        "verify".into(),
        "--scope".into(),
        "core".into(),
        "--output".into(),
        "json".into(),
    ];
    push_path(&mut args, "--path", path);
    command(args);
}

fn run_disposable_real_update(path: &Path) {
    assert!(
        path.join("config.ini").is_file(),
        "GRIFFR_LIVE_E2E_UPDATE_PATH must be a disposable existing install"
    );
    let mut before = vec![
        "info".into(),
        "--local-only".into(),
        "--output".into(),
        "json".into(),
    ];
    push_path(&mut before, "--path", path);
    let before = command(before).stdout;

    let mut update = vec![
        "--quiet".into(),
        "update".into(),
        "--resources".into(),
        "package-only".into(),
    ];
    push_path(&mut update, "--path", path);
    command(update);
    verify_core(path);

    let mut after = vec![
        "info".into(),
        "--local-only".into(),
        "--output".into(),
        "json".into(),
    ];
    push_path(&mut after, "--path", path);
    let after = command(after).stdout;
    println!("Disposable update metadata changed: {}", before != after);
}

#[test]
#[ignore = "downloads and mutates a full install through official launcher/CDN services"]
fn official_server_content_lifecycle_without_launch() {
    let config = LiveConfig::from_env();
    println!("Live E2E run root: {}", config.run_root.display());

    remote_smoke(&config);
    install_target(&config);
    verify_core(&config.primary);

    let mut info = vec![
        "info".into(),
        "--remote".into(),
        "--output".into(),
        "json".into(),
    ];
    push_path(&mut info, "--path", &config.primary);
    command(info);

    let manifest = parse_manifest(&config.primary);
    assert!(
        !manifest.is_empty(),
        "installed game_files manifest is empty"
    );
    let probe = choose_probe_entry(&config.primary, &manifest);
    let primary_probe = logical_path(&config.primary, &probe.path);
    let reuse_probe = logical_path(&config.reuse, &probe.path);
    assert_md5(&primary_probe, &probe.md5);

    copy_launcher_metadata(&config.primary, &config.reuse);
    let mut materialize = vec![
        "--quiet".into(),
        "verify".into(),
        "--repair".into(),
        "--relink-reuse".into(),
        "--scope".into(),
        "core".into(),
    ];
    push_path(&mut materialize, "--path", &config.reuse);
    push_path(&mut materialize, "--reuse-from", &config.primary);
    command(materialize);
    assert_same_hardlink(
        &primary_probe,
        &reuse_probe,
        "official install reuse should create a real same-filesystem hardlink",
    );

    let peer_bytes = fs::read(&reuse_probe).expect("read hardlinked peer before repair");
    fs::remove_file(&primary_probe).expect("remove one hardlink before repair");
    assert_eq!(
        fs::read(&reuse_probe).expect("read surviving peer"),
        peer_bytes,
        "unlinking one path must not damage its hardlinked peer"
    );

    let mut repair = vec![
        "--quiet".into(),
        "verify".into(),
        "--repair".into(),
        "--scope".into(),
        "core".into(),
    ];
    push_path(&mut repair, "--path", &config.primary);
    command(repair);
    assert_md5(&primary_probe, &probe.md5);
    assert_eq!(
        fs::read(&reuse_probe).expect("read peer after CDN repair"),
        peer_bytes,
        "repairing one install must not mutate its former hardlink peer"
    );
    assert_distinct_files(
        &primary_probe,
        &reuse_probe,
        "CDN repair should publish a new physical file after one path was removed",
    );

    let mut relink = vec![
        "--quiet".into(),
        "verify".into(),
        "--repair".into(),
        "--relink-reuse".into(),
        "--scope".into(),
        "core".into(),
    ];
    push_path(&mut relink, "--path", &config.primary);
    push_path(&mut relink, "--reuse-from", &config.reuse);
    command(relink);
    assert_same_hardlink(
        &primary_probe,
        &reuse_probe,
        "explicit relink should restore physical-file sharing",
    );

    let mut update = vec![
        "--quiet".into(),
        "update".into(),
        "--jobs".into(),
        "2".into(),
        "--resources".into(),
        "package-only".into(),
    ];
    push_path(&mut update, "--path", &config.primary);
    push_path(&mut update, "--path", &config.reuse);
    command(update);

    let mut verify_both = vec![
        "--quiet".into(),
        "verify".into(),
        "--jobs".into(),
        "2".into(),
        "--scope".into(),
        "core".into(),
        "--output".into(),
        "json".into(),
    ];
    push_path(&mut verify_both, "--path", &config.primary);
    push_path(&mut verify_both, "--path", &config.reuse);
    command(verify_both);

    if let Some(file_set) = config.resource_set.as_deref() {
        let mut resources = vec![
            "--quiet".into(),
            "resources".into(),
            "sync".into(),
            "--allow-download".into(),
            "--file-set".into(),
            file_set.into(),
        ];
        push_path(&mut resources, "--path", &config.primary);
        command(resources);

        let mut verify_all = vec![
            "--quiet".into(),
            "verify".into(),
            "--scope".into(),
            "all".into(),
            "--output".into(),
            "json".into(),
        ];
        push_path(&mut verify_all, "--path", &config.primary);
        command(verify_all);
    }

    let mut stage_inspect = vec!["--quiet".into(), "stage".into(), "inspect".into()];
    push_path(&mut stage_inspect, "--path", &config.primary);
    let inspect = run_command(stage_inspect);
    if inspect.status.success() {
        println!("{}", String::from_utf8_lossy(&inspect.stdout));
        if config.fetch_predownload {
            let stage_dir = config.run_root.join("predownload");
            let mut fetch = vec!["--quiet".into(), "stage".into(), "fetch".into()];
            push_path(&mut fetch, "--path", &config.primary);
            push_path(&mut fetch, "--stage-dir", &stage_dir);
            command(fetch);
        }
    } else {
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&inspect.stdout),
            String::from_utf8_lossy(&inspect.stderr)
        );
        assert!(
            combined.contains("No predownload payload is available"),
            "stage inspect failed unexpectedly:\n{combined}"
        );
        println!("Official server currently exposes no predownload payload.");
    }

    let mut recover = vec!["--quiet".into(), "recover".into()];
    push_path(&mut recover, "--path", &config.primary);
    command_fails_with(recover, "No install change marker found");

    if let Some(path) = config.disposable_update_path.as_deref() {
        run_disposable_real_update(path);
    }

    let mut detach = vec![
        "--quiet".into(),
        "uninstall".into(),
        "--detach".into(),
        "--yes".into(),
    ];
    push_path(&mut detach, "--path", &config.primary);
    command(detach);
    assert!(primary_probe.is_file(), "detach must keep game content");

    for target in [&config.reuse, &config.primary] {
        let mut uninstall = vec!["--quiet".into(), "uninstall".into(), "--yes".into()];
        push_path(&mut uninstall, "--path", target);
        command(uninstall);
        assert!(
            !target.exists(),
            "uninstall did not remove {}",
            target.display()
        );
    }
    fs::remove_dir(&config.run_root).expect("remove empty live E2E run directory");
}

#[compio::test]
#[ignore = "streams current official package payloads and discards each verified file"]
async fn official_server_streaming_package_soak() {
    let config = LiveConfig::from_env();
    let game: GameId = config.game.parse().expect("valid live game id");
    let region: RegionId = config.region.parse().expect("valid live region");
    let channels = ChannelPair::parse(region, config.channel.clone(), config.sub_channel.clone())
        .expect("valid live channel pair");
    let target = resolve_api_target(&game, region, &channels, &Default::default())
        .expect("resolve live API target");
    let client = ApiClient::with_user_agent(ApiClient::OFFICIAL_USER_AGENT)
        .expect("create official API client");
    let latest = client
        .get_latest_game(&target, None)
        .await
        .expect("fetch latest official package metadata");
    let package = latest
        .pkg
        .expect("official streaming soak requires a full package payload");
    assert!(
        !package.packs.is_empty(),
        "official package has no payload parts"
    );

    let stream_root = config.run_root.join("stream");
    let started = Instant::now();
    let mut verified_bytes = 0u64;
    for (index, part) in package.packs.iter().enumerate() {
        let expected_size = part.size();
        assert!(expected_size > 0, "package part {index} has no valid size");
        let logical_path = part.filename().unwrap_or("package.bin").to_string();
        let report = download_and_discard(
            ApiClient::OFFICIAL_USER_AGENT,
            &part.url,
            &logical_path,
            &part.md5,
            expected_size,
            &stream_root,
            DEFAULT_PROGRESS_BUFFER_BYTES,
        )
        .await
        .unwrap_or_else(|error| panic!("streaming package part {index} failed: {error}"));
        assert_eq!(report.bytes, expected_size, "package part {index} size");
        verified_bytes = verified_bytes.saturating_add(report.bytes);
        let mib_per_second =
            report.bytes as f64 / 1_048_576.0 / report.elapsed.as_secs_f64().max(0.001);
        println!(
            "streaming package {}/{}: {} bytes, {:.2} MiB/s, {:.3}s",
            index + 1,
            package.packs.len(),
            report.bytes,
            mib_per_second,
            report.elapsed.as_secs_f64()
        );
    }

    assert!(
        fs::read_dir(&stream_root)
            .expect("read streaming work root")
            .next()
            .is_none(),
        "streaming work root retained a payload after verification"
    );
    println!(
        "streaming package complete: {} bytes in {:.3}s ({:.2} MiB/s aggregate)",
        verified_bytes,
        started.elapsed().as_secs_f64(),
        verified_bytes as f64 / 1_048_576.0 / started.elapsed().as_secs_f64().max(0.001)
    );
    fs::remove_dir_all(&config.run_root).expect("remove empty streaming E2E run directory");
}

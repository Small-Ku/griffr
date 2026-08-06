use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use griffr_common::api::crypto::encrypt_game_files;
use md5::{Digest, Md5};
use serde_json::{json, Value};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

const EXE_NAME: &str = "Endfield.exe";
const APPCODE: &str = "6LL0KJuqHBVz33WK";

#[derive(Clone)]
struct ResourceFixture {
    version: &'static str,
    file_name: &'static str,
    file: Vec<u8>,
    encrypted_index: Vec<u8>,
}

impl ResourceFixture {
    fn new() -> Self {
        let version = "res-e2e";
        let file_name = "VFS/e2e.bin";
        let file = b"persistent-resource-e2e\n".to_vec();
        let plaintext = serde_json::to_vec(&json!({
            "version": version,
            "path": "",
            "files": [{
                "index": 0,
                "name": file_name,
                "hash": null,
                "size": file.len(),
                "type": 0,
                "md5": md5_hex(&file),
                "manifest": 0,
            }]
        }))
        .expect("serialize resource index");
        let key = griffr_common::api::crypto::RES_INDEX_KEY.as_bytes();
        let encrypted = plaintext
            .iter()
            .enumerate()
            .map(|(index, byte)| byte.wrapping_add(key[index % key.len()]))
            .collect::<Vec<_>>();
        let encrypted_index = STANDARD.encode(encrypted).into_bytes();
        Self {
            version,
            file_name,
            file,
            encrypted_index,
        }
    }
}

#[derive(Clone)]
struct ReleaseFixture {
    version: &'static str,
    rand: &'static str,
    archive_name: &'static str,
    archive: Vec<u8>,
    files: HashMap<String, Vec<u8>>,
    game_files: Vec<u8>,
    config_ini: Vec<u8>,
}

impl ReleaseFixture {
    fn new(
        temp: &Path,
        version: &'static str,
        rand: &'static str,
        archive_name: &'static str,
        core: &'static [u8],
    ) -> Self {
        let mut files = HashMap::new();
        files.insert(
            EXE_NAME.to_string(),
            format!("endfield-{version}\n").into_bytes(),
        );
        files.insert("core.bin".to_string(), core.to_vec());
        files.insert(
            "Endfield_Data/StreamingAssets/readme.txt".to_string(),
            format!("streaming-{version}\n").into_bytes(),
        );

        let archive_path = temp.join(archive_name);
        let archive_file = fs::File::create(&archive_path).expect("create fixture archive");
        let mut zip = zip::ZipWriter::new(archive_file);
        for (path, bytes) in &files {
            zip.start_file(path, SimpleFileOptions::default())
                .expect("start fixture entry");
            zip.write_all(bytes).expect("write fixture entry");
        }
        zip.finish().expect("finish fixture archive");
        let archive = fs::read(archive_path).expect("read fixture archive");

        let mut entries = files
            .iter()
            .map(|(path, bytes)| {
                json!({
                    "path": path,
                    "md5": md5_hex(bytes),
                    "size": bytes.len(),
                })
                .to_string()
            })
            .collect::<Vec<_>>();
        entries.sort();
        let game_files_plain = format!("{}\n", entries.join("\n"));
        let game_files =
            encrypt_game_files(game_files_plain.as_bytes()).expect("encrypt game_files");
        let config_plain = format!(
            "[game]\nappcode={APPCODE}\nregion=cn\nchannel=1\nsub_channel=1\nversion={version}\nentry={EXE_NAME}\n"
        );
        let config_ini = encrypt_game_files(config_plain.as_bytes()).expect("encrypt config.ini");

        Self {
            version,
            rand,
            archive_name,
            archive,
            files,
            game_files,
            config_ini,
        }
    }

    fn root(&self) -> String {
        format!("/release/{}_{}", self.version, self.rand)
    }

    fn file_path(&self, base: &str) -> String {
        format!("{base}{}/files", self.root())
    }

    fn package_json(&self, base: &str) -> Value {
        json!({
            "packs": [{
                "url": format!("{base}{}/{}", self.root(), self.archive_name),
                "md5": md5_hex(&self.archive),
                "package_size": self.archive.len().to_string(),
            }],
            "total_size": self.archive.len().to_string(),
            "file_path": self.file_path(base),
            "game_files_md5": md5_hex(&self.game_files),
        })
    }
}

struct FixtureServer {
    base: String,
    selected_release: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FixtureServer {
    fn start(v1: ReleaseFixture, v2: ReleaseFixture, resources: ResourceFixture) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        listener
            .set_nonblocking(true)
            .expect("set fixture server nonblocking");
        let base = format!("http://{}", listener.local_addr().expect("fixture address"));
        let thread_base = base.clone();
        let selected_release = Arc::new(AtomicUsize::new(1));
        let selected_thread = Arc::clone(&selected_release);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);

        let thread = thread::spawn(move || {
            while !stop_thread.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let release = if selected_thread.load(Ordering::Acquire) == 1 {
                            &v1
                        } else {
                            &v2
                        };
                        handle_connection(stream, &thread_base, release, &v2, &resources);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base,
            selected_release,
            stop,
            thread: Some(thread),
        }
    }

    fn select_v2(&self) {
        self.selected_release.store(2, Ordering::Release);
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    base: &str,
    release: &ReleaseFixture,
    v2: &ReleaseFixture,
    resources: &ResourceFixture,
) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let first = request.lines().next().unwrap_or_default();
    let mut first_parts = first.split_whitespace();
    let method = first_parts.next().unwrap_or("GET");
    let target = first_parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or(target);

    if path == "/api/proxy/batch_proxy" {
        let request_version = request
            .split_once("\r\n\r\n")
            .and_then(|(_, body)| serde_json::from_str::<Value>(body).ok())
            .and_then(|value| {
                value["proxy_reqs"][0]["get_latest_game_req"]["version"]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let action = i32::from(request_version != release.version);
        let patch = (release.version == v2.version && request_version == "1.0.0").then(|| {
            json!({
                "url": format!("{base}{}/{}", v2.root(), v2.archive_name),
                "md5": md5_hex(&v2.archive),
                "file_id": "e2e-patch",
                "cd_key": null,
                "patches": [{
                    "url": format!("{base}{}/{}", v2.root(), v2.archive_name),
                    "md5": md5_hex(&v2.archive),
                    "package_size": v2.archive.len().to_string(),
                }],
                "package_size": v2.archive.len().to_string(),
                "total_size": v2.archive.len().to_string(),
            })
        });
        let pre_patch = (release.version == v2.version).then(|| {
            json!({
                "version": v2.version,
                "patches": [{
                    "url": format!("{base}{}/{}", v2.root(), v2.archive_name),
                    "md5": md5_hex(&v2.archive),
                    "package_size": v2.archive.len().to_string(),
                }],
                "package_size": v2.archive.len().to_string(),
                "total_size": v2.archive.len().to_string(),
            })
        });
        let body = json!({
            "proxy_rsps": [{
                "kind": "get_latest_game",
                "get_latest_game_rsp": {
                    "action": action,
                    "request_version": request_version,
                    "version": release.version,
                    "pkg": release.package_json(base),
                    "patch": patch,
                    "pre_patch": pre_patch,
                    "state": 0,
                    "launcher_action": 0,
                }
            }]
        });
        write_json(&mut stream, &body);
        return;
    }

    if path == "/api/proxy/web/batch_proxy" {
        let body = json!({
            "proxy_rsps": [
                {"kind":"get_banner","get_banner_rsp":{"data_version":"1","banners":[]}},
                {"kind":"get_announcement","get_announcement_rsp":{"data_version":"1","tabs":[]}},
                {"kind":"get_main_bg_image","get_main_bg_image_rsp":{"data_version":"1","main_bg_image":{"url":"","md5":"","video_url":""}}},
                {"kind":"get_sidebar","get_sidebar_rsp":{"data_version":"1","sidebars":[]}}
            ]
        });
        write_json(&mut stream, &body);
        return;
    }

    if path == "/api/game/get_latest_resources" {
        let body = json!({
            "resources": [{
                "name": "initial",
                "version": resources.version,
                "path": format!("{base}/resources/initial"),
            }],
            "configs": "{}",
            "res_version": format!("initial_{}", resources.version),
            "patch_index_path": "",
            "domain": base,
        });
        write_json(&mut stream, &body);
        return;
    }

    let static_body = static_route(path, release, v2, resources);
    match static_body {
        Some(body) => write_bytes(&mut stream, method, &request, &body),
        None => write_response(&mut stream, "404 Not Found", b"not found", &[]),
    }
}

fn static_route(
    path: &str,
    selected: &ReleaseFixture,
    v2: &ReleaseFixture,
    resources: &ResourceFixture,
) -> Option<Vec<u8>> {
    if matches!(
        path,
        "/resources/initial/index_initial.json" | "/resources/initial/pref_initial.json"
    ) {
        return Some(resources.encrypted_index.clone());
    }
    if path == format!("/resources/initial/{}", resources.file_name) {
        return Some(resources.file.clone());
    }
    for release in [selected, v2] {
        let root = release.root();
        if path == format!("{root}/{}", release.archive_name) {
            return Some(release.archive.clone());
        }
        if path == format!("{root}/files/game_files") {
            return Some(release.game_files.clone());
        }
        if path == format!("{root}/files/config.ini") {
            return Some(release.config_ini.clone());
        }
        if path == format!("{root}/files/package_files") {
            return Some(Vec::new());
        }
        if let Some(logical) = path.strip_prefix(&format!("{root}/files/")) {
            if let Some(bytes) = release.files.get(logical) {
                return Some(bytes.clone());
            }
        }
    }
    None
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(header_end) = find_bytes(&bytes, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).ok()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_json(stream: &mut TcpStream, value: &Value) {
    let bytes = serde_json::to_vec(value).expect("serialize fixture JSON");
    write_response(
        stream,
        "200 OK",
        &bytes,
        &[("Content-Type", "application/json")],
    );
}

fn write_bytes(stream: &mut TcpStream, method: &str, request: &str, body: &[u8]) {
    let range = request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("range") {
            return None;
        }
        let value = value.trim().strip_prefix("bytes=")?;
        let (start, end) = value.split_once('-')?;
        let start = start.parse::<usize>().ok()?;
        let end = (!end.is_empty())
            .then(|| end.parse::<usize>().ok())
            .flatten();
        Some((start, end))
    });

    if let Some((start, end)) = range {
        let start = start.min(body.len());
        let end = end
            .unwrap_or_else(|| body.len().saturating_sub(1))
            .min(body.len().saturating_sub(1));
        let slice = if start <= end {
            &body[start..=end]
        } else {
            &[]
        };
        let headers = [(
            "Content-Range".to_string(),
            format!("bytes {start}-{end}/{}", body.len()),
        )];
        write_response_owned(stream, "206 Partial Content", method, slice, &headers);
    } else {
        write_response_owned(stream, "200 OK", method, body, &[]);
    }
}

fn write_response(stream: &mut TcpStream, status: &str, body: &[u8], headers: &[(&str, &str)]) {
    let owned = headers
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect::<Vec<_>>();
    write_response_owned(stream, status, "GET", body, &owned);
}

fn write_response_owned(
    stream: &mut TcpStream,
    status: &str,
    method: &str,
    body: &[u8],
    headers: &[(String, String)],
) {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes()).ok();
    if method != "HEAD" {
        stream.write_all(body).ok();
    }
}

fn md5_hex(bytes: &[u8]) -> String {
    griffr_common::to_hex(&Md5::digest(bytes))
}

fn run_command<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_griffr"))
        .args(args)
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
    output
}

fn command_fails<I, S>(args: I, expected: &str) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_command(args);
    assert!(
        !output.status.success(),
        "griffr unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains(expected),
        "expected failure containing {expected:?}, got:\n{combined}"
    );
    output
}

fn args(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

fn push_path(values: &mut Vec<OsString>, flag: &str, path: &Path) {
    values.push(flag.into());
    values.push(path.as_os_str().to_owned());
}

#[test]
fn every_command_surface_has_a_working_help_contract() {
    let commands: &[&[&str]] = &[
        &["--help"],
        &["install", "--help"],
        &["uninstall", "--help"],
        &["update", "--help"],
        &["stage", "--help"],
        &["stage", "inspect", "--help"],
        &["stage", "fetch", "--help"],
        &["stage", "apply", "--help"],
        &["stage", "resume", "--help"],
        &["recover", "--help"],
        &["launch", "--help"],
        &["verify", "--help"],
        &["resources", "--help"],
        &["resources", "sync", "--help"],
        &["setup-persistent-resources", "--help"],
        &["info", "--help"],
        &["news", "--help"],
        &["debug", "--help"],
        &["debug", "detect-config-ini", "--help"],
        &["debug", "decrypt-config-ini", "--help"],
        &["debug", "decrypt-game-files", "--help"],
        &["debug", "decrypt-res-index", "--help"],
        &["debug", "vfs-diff", "--help"],
        &["debug", "snapshot-resource-state", "--help"],
        &["debug", "diff-resource-snapshots", "--help"],
        &["debug", "get-raw-latest-game", "--help"],
        &["debug", "get-raw-latest-resources", "--help"],
        &["debug", "list-game-files", "--help"],
        &["debug", "list-resource-files", "--help"],
        &["debug", "get-file", "--help"],
        &["debug", "get-raw-media", "--help"],
        &["debug", "get-media", "--help"],
        &["account", "--help"],
        &["account", "capture", "--help"],
        &["account", "activate", "--help"],
    ];

    for command_line in commands {
        command(args(command_line));
    }
}

#[test]
fn local_launcher_service_drives_install_repair_stage_update_and_uninstall() {
    let temp = TempDir::new().expect("test tempdir");
    let v1 = ReleaseFixture::new(
        temp.path(),
        "1.0.0",
        "e2ev1",
        "endfield-v1.zip",
        b"core-version-one\n",
    );
    let v2 = ReleaseFixture::new(
        temp.path(),
        "1.1.0",
        "e2ev2",
        "endfield-v2.zip",
        b"core-version-two-with-more-data\n",
    );
    let resources = ResourceFixture::new();
    let server = FixtureServer::start(v1.clone(), v2.clone(), resources.clone());
    let install = temp.path().join("install");

    let mut install_args = args(&[
        "--quiet",
        "install",
        "--game",
        "endfield",
        "--region",
        "cn",
        "--resources",
        "package-only",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut install_args, "--path", &install);
    command(install_args);
    assert_eq!(
        fs::read(install.join("core.bin")).unwrap(),
        v1.files["core.bin"]
    );
    assert!(install.join("config.ini").is_file());
    assert!(install.join("game_files").is_file());

    let mut info_args = args(&["info", "--local-only", "--output", "json"]);
    push_path(&mut info_args, "--path", &install);
    let info = command(info_args);
    let info_text = String::from_utf8_lossy(&info.stdout);
    assert!(info_text.contains("1.0.0"), "{info_text}");

    for debug_command in [
        "detect-config-ini",
        "decrypt-config-ini",
        "decrypt-game-files",
    ] {
        let mut debug_args = args(&["debug", debug_command]);
        push_path(&mut debug_args, "--path", &install);
        command(debug_args);
    }

    let mut verify_args = args(&[
        "--quiet",
        "verify",
        "--scope",
        "core",
        "--output",
        "json",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut verify_args, "--path", &install);
    command(verify_args);

    fs::write(install.join("core.bin"), b"corrupt").unwrap();
    let mut repair_args = args(&[
        "--quiet",
        "verify",
        "--repair",
        "--scope",
        "core",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut repair_args, "--path", &install);
    command(repair_args);
    assert_eq!(
        fs::read(install.join("core.bin")).unwrap(),
        v1.files["core.bin"]
    );

    let reuse_install = temp.path().join("reuse-install");
    fs::create_dir_all(&reuse_install).unwrap();
    fs::copy(install.join("config.ini"), reuse_install.join("config.ini")).unwrap();
    fs::copy(install.join("game_files"), reuse_install.join("game_files")).unwrap();
    let mut reuse_repair_args = args(&[
        "--quiet",
        "verify",
        "--repair",
        "--relink-reuse",
        "--scope",
        "core",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut reuse_repair_args, "--path", &reuse_install);
    push_path(&mut reuse_repair_args, "--reuse-from", &install);
    command(reuse_repair_args);
    assert_eq!(
        fs::read(reuse_install.join("core.bin")).unwrap(),
        v1.files["core.bin"]
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(install.join("core.bin")).unwrap().ino(),
            fs::metadata(reuse_install.join("core.bin")).unwrap().ino(),
            "same-volume explicit reuse should hardlink matching files"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let launch_log = temp.path().join("wine.log");
        let runner = temp.path().join("fake-wine.sh");
        fs::write(
            &runner,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                launch_log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&runner).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&runner, permissions).unwrap();

        let mut launch_args = args(&["--quiet", "launch"]);
        push_path(&mut launch_args, "--path", &install);
        push_path(&mut launch_args, "--wine", &runner);
        command(launch_args);
        for _ in 0..100 {
            if launch_log.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(launch_log.exists());
        assert!(fs::read_to_string(launch_log).unwrap().contains(EXE_NAME));
    }

    let sdk_source = temp.path().join("sdk_data_e2e_source");
    let sdk_target = temp.path().join("sdk_data_e2e_target");
    let account_bundle = temp.path().join("account-bundle");
    fs::create_dir_all(&sdk_source).unwrap();
    fs::write(sdk_source.join("session.dat"), b"session-one").unwrap();
    fs::create_dir_all(install.join("mmkv")).unwrap();
    fs::write(install.join("mmkv/account"), b"account-one").unwrap();

    let mut capture_args = args(&[
        "--quiet",
        "account",
        "capture",
        "endfield",
        "--include-install-mmkv",
    ]);
    push_path(&mut capture_args, "--to", &account_bundle);
    push_path(&mut capture_args, "--sdk-dir", &sdk_source);
    push_path(&mut capture_args, "--install-path", &install);
    command(capture_args);

    let mut activate_args = args(&[
        "--quiet",
        "account",
        "activate",
        "endfield",
        "--include-install-mmkv",
        "--force",
    ]);
    push_path(&mut activate_args, "--from", &account_bundle);
    push_path(&mut activate_args, "--sdk-dir", &sdk_target);
    push_path(&mut activate_args, "--install-path", &install);
    command(activate_args);
    assert_eq!(
        fs::read(sdk_target.join("session.dat")).unwrap(),
        b"session-one"
    );

    let remote_prefix = [
        "--game",
        "endfield",
        "--region",
        "cn",
        "--gateway",
        server.base.as_str(),
    ];
    command([vec![OsString::from("news")], args(&remote_prefix)].concat());
    for debug_remote in [
        "get-raw-latest-game",
        "get-raw-latest-resources",
        "list-game-files",
        "list-resource-files",
        "get-raw-media",
        "get-media",
    ] {
        let mut values = vec!["debug".into(), debug_remote.into()];
        values.extend(args(&remote_prefix));
        command(values);
    }
    let fetched = temp.path().join("fetched-core.bin");
    let mut get_file_args = vec!["debug".into(), "get-file".into()];
    get_file_args.extend(args(&remote_prefix));
    get_file_args.extend(["--file".into(), "core.bin".into()]);
    push_path(&mut get_file_args, "--output-file", &fetched);
    command(get_file_args);
    assert_eq!(fs::read(fetched).unwrap(), v1.files["core.bin"]);

    server.select_v2();
    let stage = temp.path().join("stage");
    for action in ["inspect", "fetch"] {
        let mut stage_args = args(&["--quiet", "stage", action, "--gateway", &server.base]);
        push_path(&mut stage_args, "--path", &install);
        if action == "fetch" {
            push_path(&mut stage_args, "--stage-dir", &stage);
        }
        command(stage_args);
    }
    assert!(stage.join(v2.archive_name).is_file());

    let mut stage_apply_args = args(&[
        "--quiet",
        "stage",
        "apply",
        "--resources",
        "package-only",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut stage_apply_args, "--path", &install);
    push_path(&mut stage_apply_args, "--stage-dir", &stage);
    command(stage_apply_args);
    assert_eq!(
        fs::read(install.join("core.bin")).unwrap(),
        v2.files["core.bin"]
    );
    assert_eq!(
        fs::read(reuse_install.join("core.bin")).unwrap(),
        v1.files["core.bin"],
        "updating a hardlinked peer must replace files instead of mutating shared inodes"
    );

    let mut update_args = args(&[
        "--quiet",
        "update",
        "--resources",
        "package-only",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut update_args, "--path", &reuse_install);
    push_path(&mut update_args, "--reuse-from", &install);
    command(update_args);
    assert_eq!(
        fs::read(reuse_install.join("core.bin")).unwrap(),
        v2.files["core.bin"]
    );

    let mut verify_v2 = args(&[
        "--quiet",
        "verify",
        "--jobs",
        "2",
        "--scope",
        "core",
        "--output",
        "json",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut verify_v2, "--path", &install);
    push_path(&mut verify_v2, "--path", &reuse_install);
    command(verify_v2);

    let persistent = install.join("Endfield_Data/Persistent");
    let mut resource_args = args(&[
        "--quiet",
        "resources",
        "sync",
        "--allow-download",
        "--file-set",
        "initial",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut resource_args, "--path", &install);
    command(resource_args);
    let resource_path = persistent.join(resources.file_name);
    assert_eq!(fs::read(&resource_path).unwrap(), resources.file);

    let mut legacy_resource_args = args(&[
        "--quiet",
        "setup-persistent-resources",
        "--allow-download",
        "--file-set",
        "initial",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut legacy_resource_args, "--path", &install);
    command(legacy_resource_args);

    let mut decrypt_index_args = args(&["debug", "decrypt-res-index"]);
    push_path(&mut decrypt_index_args, "--path", &persistent);
    command(decrypt_index_args);
    let mut vfs_diff_args = args(&["debug", "vfs-diff"]);
    push_path(&mut vfs_diff_args, "--path", &persistent);
    command(vfs_diff_args);

    let before_snapshot = temp.path().join("resource-before.json");
    let after_snapshot = temp.path().join("resource-after.json");
    for output in [&before_snapshot, &after_snapshot] {
        if output == &after_snapshot {
            fs::write(&resource_path, b"mutated-resource").unwrap();
        }
        let mut snapshot_args = args(&[
            "debug",
            "snapshot-resource-state",
            "--hash-check",
            "persistent",
        ]);
        push_path(&mut snapshot_args, "--path", &install);
        push_path(&mut snapshot_args, "--output-file", output);
        command(snapshot_args);
    }
    let mut diff_args = args(&["debug", "diff-resource-snapshots"]);
    push_path(&mut diff_args, "--before", &before_snapshot);
    push_path(&mut diff_args, "--after", &after_snapshot);
    command(diff_args);

    let mut repair_resource_args = args(&[
        "--quiet",
        "resources",
        "sync",
        "--allow-download",
        "--file-set",
        "initial",
        "--gateway",
        &server.base,
    ]);
    push_path(&mut repair_resource_args, "--path", &install);
    command(repair_resource_args);
    assert_eq!(fs::read(&resource_path).unwrap(), resources.file);

    let mut recover_args = args(&["--quiet", "recover"]);
    push_path(&mut recover_args, "--path", &install);
    command_fails(recover_args, "No install change marker found");

    let mut legacy_resume_args = args(&["--quiet", "stage", "resume"]);
    push_path(&mut legacy_resume_args, "--path", &install);
    command_fails(legacy_resume_args, "No install change marker found");

    let mut detach_args = args(&["--quiet", "uninstall", "--detach", "--yes"]);
    push_path(&mut detach_args, "--path", &install);
    command(detach_args);
    assert!(install.join("core.bin").is_file());

    for target in [&reuse_install, &install] {
        let mut uninstall_args = args(&["--quiet", "uninstall", "--yes"]);
        push_path(&mut uninstall_args, "--path", target);
        command(uninstall_args);
        assert!(!target.exists());
    }
}

#[test]
fn resource_index_cipher_fixture_round_trips() {
    let plaintext = br#"{"version":"1","path":"","files":[]}"#;
    let key = griffr_common::api::crypto::RES_INDEX_KEY.as_bytes();
    let encrypted = plaintext
        .iter()
        .enumerate()
        .map(|(index, byte)| byte.wrapping_add(key[index % key.len()]))
        .collect::<Vec<_>>();
    let encoded = STANDARD.encode(encrypted);
    assert_eq!(
        griffr_common::api::crypto::decrypt_res_index(
            &encoded,
            griffr_common::api::crypto::RES_INDEX_KEY,
        )
        .unwrap()
        .as_bytes(),
        plaintext
    );
}

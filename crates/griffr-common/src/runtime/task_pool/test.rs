use super::*;
use crate::runtime::task_pool::download::do_download;
use crate::runtime::task_pool::fs_ops::{
    make_partial_download_path, make_temp_write_path, write_file,
};
use compio::dispatcher::Dispatcher;
use md5::{Digest, Md5};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;
use zip::write::FileOptions;

#[test]
fn test_make_temp_write_path_stays_in_parent_dir() {
    let target = PathBuf::from("target").join("Endfield.exe");
    let temp = make_temp_write_path(&target).unwrap();
    assert_eq!(temp.parent(), target.parent());
    let name = temp.file_name().unwrap().to_string_lossy();
    assert!(name.starts_with(".Endfield.exe.griffr.tmp."));
}

#[test]
fn test_write_file_replaces_hardlink_instead_of_mutating_shared_inode() {
    let tmp = tempdir().unwrap();
    let original = tmp.path().join("original.bin");
    let linked = tmp.path().join("linked.bin");
    std::fs::write(&original, b"before").unwrap();
    std::fs::hard_link(&original, &linked).unwrap();
    assert_eq!(std::fs::read(&original).unwrap(), b"before");
    assert_eq!(std::fs::read(&linked).unwrap(), b"before");

    let dispatcher = Dispatcher::builder()
        .worker_threads(NonZeroUsize::new(1).unwrap())
        .build()
        .expect("dispatcher should build");
    write_file(Some(&dispatcher), &linked, b"after".to_vec()).unwrap();

    assert_eq!(std::fs::read(&linked).unwrap(), b"after");
    assert_eq!(
        std::fs::read(&original).unwrap(),
        b"before",
        "writing linked path must not mutate the original hardlinked file"
    );
}

#[test]
fn ensure_file_can_relink_verified_target_when_prefer_reuse_enabled() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let target = tmp.path().join("target.bin");
    std::fs::write(&source, b"same-bytes").unwrap();
    std::fs::write(&target, b"same-bytes").unwrap();
    let expected_md5 = format!("{:x}", Md5::digest(b"same-bytes"));

    let tasks = vec![Task::EnsureFile {
        dest: target.clone(),
        logical_path: "target.bin".to_string(),
        expected_md5,
        expected_size: 10,
        source_candidates: vec![source.clone()],
        download_url: None,
        allow_copy_fallback: false,
        prefer_reuse: true,
        retry_count: 0,
    }];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, ProgressEvent::Hardlinked { .. })),
        "expected hardlink event when prefer_reuse is enabled"
    );
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            ProgressEvent::Verified {
                ok: true,
                issue: None,
                ..
            }
        )),
        "expected verify success after relink"
    );
}

fn start_test_http_server(
    routes: HashMap<String, Vec<u8>>,
) -> (String, Arc<Mutex<HashMap<String, usize>>>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking test server");
    let addr = listener.local_addr().expect("server addr");
    let hits = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let hits_thread = Arc::clone(&hits);
    let stop_thread = Arc::clone(&stop);

    thread::spawn(move || {
        while !stop_thread.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 4096];
                    let read = stream.read(&mut buf).unwrap_or(0);
                    if read == 0 {
                        continue;
                    }
                    let req = String::from_utf8_lossy(&buf[..read]);
                    let first_line = req.lines().next().unwrap_or_default();
                    let path = first_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();

                    {
                        let mut guard = hits_thread.lock().unwrap();
                        *guard.entry(path.clone()).or_insert(0) += 1;
                    }

                    if let Some(body) = routes.get(&path) {
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body);
                    } else {
                        let body = b"not found";
                        let header = format!(
                                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    (format!("http://{}", addr), hits, stop)
}

fn start_range_http_server(
    path: &'static str,
    body: Vec<u8>,
) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking test server");
    let addr = listener.local_addr().expect("server addr");
    let range_hits = Arc::new(AtomicUsize::new(0));
    let total_hits = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let range_hits_thread = Arc::clone(&range_hits);
    let total_hits_thread = Arc::clone(&total_hits);
    let stop_thread = Arc::clone(&stop);

    thread::spawn(move || {
        while !stop_thread.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 8192];
                    let read = stream.read(&mut buf).unwrap_or(0);
                    if read == 0 {
                        continue;
                    }
                    total_hits_thread.fetch_add(1, Ordering::AcqRel);
                    let req = String::from_utf8_lossy(&buf[..read]);
                    let mut lines = req.lines();
                    let first_line = lines.next().unwrap_or_default();
                    let req_path = first_line.split_whitespace().nth(1).unwrap_or("/");
                    if req_path != path {
                        let body = b"not found";
                        let header = format!(
                                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body);
                        continue;
                    }

                    let mut range_start = None::<usize>;
                    for line in lines {
                        let lower = line.to_ascii_lowercase();
                        if let Some(rest) = lower.strip_prefix("range: bytes=") {
                            if let Some((start, _end)) = rest.split_once('-') {
                                if let Ok(parsed) = start.trim().parse::<usize>() {
                                    range_start = Some(parsed);
                                }
                            }
                            break;
                        }
                    }

                    if let Some(start) = range_start {
                        range_hits_thread.fetch_add(1, Ordering::AcqRel);
                        let start = start.min(body.len());
                        let resp = &body[start..];
                        let content_range = format!(
                            "bytes {}-{}/{}",
                            start,
                            body.len().saturating_sub(1),
                            body.len()
                        );
                        let header = format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: {}\r\nConnection: close\r\n\r\n",
                                resp.len(),
                                content_range
                            );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(resp);
                    } else {
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    (format!("http://{}", addr), range_hits, total_hits, stop)
}

#[test]
fn do_download_resume_incremental_md5_produces_correct_result() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("asset.chk");
    let part = make_partial_download_path(&dest).unwrap();

    let mut payload = Vec::with_capacity(2 * 1024 * 1024 + 333);
    for i in 0..(2 * 1024 * 1024 + 333) {
        payload.push((i % 251) as u8);
    }
    let expected_md5 = format!("{:x}", Md5::digest(&payload));

    let cut = 1_048_576usize;
    std::fs::write(&part, &payload[..cut]).unwrap();

    let (base, range_hits, total_hits, stop) = start_range_http_server("/blob", payload.clone());
    let url = format!("{}/blob", base);

    let dispatcher = Dispatcher::builder()
        .worker_threads(NonZeroUsize::new(2).unwrap())
        .build()
        .expect("dispatcher should build");
    let len = do_download(
        Some(&dispatcher),
        "Mozilla/5.0",
        &url,
        &dest,
        &expected_md5,
        Some(payload.len() as u64),
        0,
        None::<fn(u64)>,
    )
    .unwrap();

    stop.store(true, Ordering::Release);

    assert_eq!(len, payload.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(
        !part.exists(),
        "partial file should be promoted and removed after successful commit"
    );
    assert!(
        total_hits.load(Ordering::Acquire) >= 1,
        "expected at least one request"
    );
    assert!(
        range_hits.load(Ordering::Acquire) >= 1,
        "expected resume request with Range header"
    );
}

#[test]
fn install_archive_recovers_from_interrupted_partial_part_on_rerun() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("downloads");
    let install_dir = tmp.path().join("install");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();

    let zip_path = tmp.path().join("bundle.zip");
    let zip_file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file("data.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"recovered after interruption").unwrap();
    zip.finish().unwrap();

    let zip_bytes = std::fs::read(&zip_path).unwrap();
    let split_at = (zip_bytes.len() / 2).max(1);
    let part1 = zip_bytes[..split_at].to_vec();
    let part2 = zip_bytes[split_at..].to_vec();
    assert!(!part2.is_empty());

    let part1_path = source_dir.join("bundle.zip.001");
    let part2_path = source_dir.join("bundle.zip.002");
    // Simulate rerun after interruption:
    // - first part already complete
    // - second part partially written/corrupted
    std::fs::write(&part1_path, &part1).unwrap();
    std::fs::write(&part2_path, &part2[..(part2.len() / 2).max(1)]).unwrap();

    let mut routes = HashMap::new();
    routes.insert("/bundle.zip.001".to_string(), part1.clone());
    routes.insert("/bundle.zip.002".to_string(), part2.clone());
    let (base_url, hits, stop) = start_test_http_server(routes);

    let tasks = vec![Task::InstallArchive {
        source_dir: source_dir.clone(),
        base_name: "bundle".to_string(),
        dest: install_dir.clone(),
        cleanup: false,
        password: None,
        parts: vec![
            ArchivePart {
                url: format!("{}/bundle.zip.001", base_url),
                dest: part1_path.clone(),
                logical_path: "bundle.zip.001".to_string(),
                expected_md5: format!("{:x}", Md5::digest(&part1)),
                expected_size: part1.len() as u64,
            },
            ArchivePart {
                url: format!("{}/bundle.zip.002", base_url),
                dest: part2_path.clone(),
                logical_path: "bundle.zip.002".to_string(),
                expected_md5: format!("{:x}", Md5::digest(&part2)),
                expected_size: part2.len() as u64,
            },
        ],
    }];

    let mut cfg = TaskPoolConfig::default();
    cfg.max_retries = 1;
    let result = run_tasks(tasks, cfg).unwrap();
    stop.store(true, Ordering::Release);

    let downloaded = result
        .events
        .iter()
        .filter_map(|event| match event {
            ProgressEvent::Downloaded { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        downloaded,
        vec!["bundle.zip.002".to_string()],
        "rerun recovery should only redownload the corrupted partial part"
    );
    assert!(
        result
            .events
            .iter()
            .any(|event| matches!(event, ProgressEvent::Extracted { .. })),
        "archive should extract after recovering the missing/corrupt part"
    );

    let guard = hits.lock().unwrap();
    assert_eq!(
        guard.get("/bundle.zip.001").copied().unwrap_or(0),
        0,
        "valid completed part should be reused without HTTP download"
    );
    assert_eq!(
        guard.get("/bundle.zip.002").copied().unwrap_or(0),
        1,
        "corrupted partial part should be downloaded once"
    );

    let extracted = std::fs::read_to_string(install_dir.join("data.txt")).unwrap();
    assert_eq!(extracted, "recovered after interruption");
}

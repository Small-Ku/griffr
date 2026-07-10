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

mod archive;
mod download;

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

use super::*;
fn start_test_http_channel(
    routes: HashMap<String, Vec<u8>>,
) -> (String, Arc<Mutex<HashMap<String, usize>>>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test channel");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking test channel");
    let addr = listener.local_addr().expect("channel addr");
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

#[compio::test]
async fn download_and_extract_archives_recovers_partial_part_on_rerun() {
    let tmp = tempdir().unwrap();
    let install_path = tmp.path().join("install");
    let download_dir = install_path.join("downloads");
    std::fs::create_dir_all(&download_dir).unwrap();
    std::fs::create_dir_all(&install_path).unwrap();

    let zip_path = tmp.path().join("bundle.zip");
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("payload.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"cli updater recovery").unwrap();
    zip.finish().unwrap();
    let zip_bytes = std::fs::read(&zip_path).unwrap();

    let split_at = (zip_bytes.len() / 2).max(1);
    let part1 = zip_bytes[..split_at].to_vec();
    let part2 = zip_bytes[split_at..].to_vec();
    assert!(!part2.is_empty());

    // Simulate interrupted prior run:
    // - part1 fully downloaded and valid
    // - part2 truncated/corrupted
    let part1_name = "bundle.zip.001";
    let part2_name = "bundle.zip.002";
    std::fs::write(download_dir.join(part1_name), &part1).unwrap();
    std::fs::write(
        download_dir.join(part2_name),
        &part2[..(part2.len() / 2).max(1)],
    )
    .unwrap();

    let mut routes = HashMap::new();
    routes.insert(format!("/{}", part1_name), part1.clone());
    routes.insert(format!("/{}", part2_name), part2.clone());
    let (base_url, hits, stop) = start_test_http_channel(routes);

    let archives = vec![
        PackFile {
            url: format!("{}/{}", base_url, part1_name),
            md5: format!("{:x}", md5::Md5::digest(&part1)),
            package_size: part1.len().to_string(),
        },
        PackFile {
            url: format!("{}/{}", base_url, part2_name),
            md5: format!("{:x}", md5::Md5::digest(&part2)),
            package_size: part2.len().to_string(),
        },
    ];

    let opts = test_global_options();

    let pool_cfg = TaskPoolConfig::with_progress_buffers(
        opts.extraction_progress_buffer_bytes,
        opts.download_progress_buffer_bytes,
    );
    let mut pool_runner = TaskPoolRunner::new(pool_cfg).unwrap();
    let result = download_and_extract_archives(
        &archives,
        &install_path,
        "patch",
        false,
        None,
        &griffr_common::runtime::PatchApplyOptions::default(),
        &opts,
        &mut pool_runner,
    )
    .await;
    stop.store(true, Ordering::Release);
    let modified_paths = result.unwrap();
    assert!(modified_paths.iter().any(|path| path == "payload.txt"));

    let guard = hits.lock().unwrap();
    assert_eq!(
        guard.get(&format!("/{}", part1_name)).copied().unwrap_or(0),
        0,
        "valid part should be reused and skipped"
    );
    assert_eq!(
        guard.get(&format!("/{}", part2_name)).copied().unwrap_or(0),
        1,
        "truncated part should be fetched once on rerun"
    );
    drop(guard);

    let extracted = std::fs::read_to_string(install_path.join("payload.txt")).unwrap();
    assert_eq!(extracted, "cli updater recovery");
}

#[compio::test]
async fn download_and_extract_archives_applies_delete_files_manifest() {
    let tmp = tempdir().unwrap();
    let install_path = tmp.path().join("install");
    std::fs::create_dir_all(install_path.join("Endfield_Data/Plugins/x86_64")).unwrap();
    std::fs::write(
        install_path.join("Endfield_Data/Plugins/x86_64/libHAPI.dll"),
        b"obsolete",
    )
    .unwrap();

    let zip_path = tmp.path().join("bundle.zip");
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("payload.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"updated payload").unwrap();
    zip.start_file("delete_files.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"Endfield_Data/Plugins/x86_64/libHAPI.dll\n")
        .unwrap();
    zip.finish().unwrap();
    let zip_bytes = std::fs::read(&zip_path).unwrap();

    let part_name = "bundle.zip.001";
    let mut routes = HashMap::new();
    routes.insert(format!("/{}", part_name), zip_bytes.clone());
    let (base_url, _hits, stop) = start_test_http_channel(routes);

    let archives = vec![PackFile {
        url: format!("{}/{}", base_url, part_name),
        md5: format!("{:x}", md5::Md5::digest(&zip_bytes)),
        package_size: zip_bytes.len().to_string(),
    }];

    let opts = test_global_options();
    let pool_cfg = TaskPoolConfig::with_progress_buffers(
        opts.extraction_progress_buffer_bytes,
        opts.download_progress_buffer_bytes,
    );
    let mut pool_runner = TaskPoolRunner::new(pool_cfg).unwrap();

    let result = download_and_extract_archives(
        &archives,
        &install_path,
        "patch",
        false,
        None,
        &griffr_common::runtime::PatchApplyOptions::default(),
        &opts,
        &mut pool_runner,
    )
    .await;
    stop.store(true, Ordering::Release);
    let modified_paths = result.unwrap();
    assert!(modified_paths.iter().any(|path| path == "payload.txt"));

    assert_eq!(
        std::fs::read_to_string(install_path.join("payload.txt")).unwrap(),
        "updated payload"
    );
    assert!(!install_path
        .join("Endfield_Data/Plugins/x86_64/libHAPI.dll")
        .exists());
    assert!(!install_path.join("delete_files.txt").exists());
}

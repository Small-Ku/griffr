use super::*;

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
    std::fs::write(&part1_path, &part1).unwrap();
    std::fs::write(&part2_path, &part2[..(part2.len() / 2).max(1)]).unwrap();

    let mut routes = HashMap::new();
    routes.insert("/bundle.zip.001".to_string(), part1.clone());
    routes.insert("/bundle.zip.002".to_string(), part2.clone());
    let (base_url, hits, stop) = start_test_http_channel(routes);

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

    let cfg = TaskPoolConfig {
        max_retries: 1,
        ..Default::default()
    };
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

#[test]
fn extract_task_spawns_vfs_patch_and_delete_manifest_follow_up_tasks() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("downloads");
    let install_dir = tmp.path().join("install");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(install_dir.join("Endfield_Data/Plugins/x86_64")).unwrap();
    std::fs::write(
        install_dir.join("Endfield_Data/Plugins/x86_64/libHAPI.dll"),
        b"obsolete",
    )
    .unwrap();

    let zip_path = tmp.path().join("bundle.zip");
    let zip_file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file("payload.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"updated payload").unwrap();
    zip.start_file("patch.json", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(
        br#"{
  "version": "75.0.0",
  "vfs_base_path": "Arknights_Data/StreamingAssets/AB/Windows",
  "files": [
    {
      "name": "ui/direct.ab",
      "md5": "75c4e133155014e946c3ef39652b0ba8",
      "size": 13,
      "local_path": "files/ui/direct.ab",
      "diffType": 0,
      "patch": []
    }
  ]
}"#,
    )
    .unwrap();
    zip.start_file("vfs_files/files/ui/direct.ab", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"patched bytes").unwrap();
    zip.start_file("delete_files.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"Endfield_Data/Plugins/x86_64/libHAPI.dll\n")
        .unwrap();
    zip.finish().unwrap();

    let zip_bytes = std::fs::read(&zip_path).unwrap();
    std::fs::write(source_dir.join("bundle.zip.001"), &zip_bytes).unwrap();

    let tasks = vec![Task::Extract {
        source_dir: source_dir.clone(),
        base_name: "bundle".to_string(),
        dest: install_dir.clone(),
        cleanup: false,
        password: None,
    }];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();

    assert!(
        result
            .events
            .iter()
            .all(|event| !matches!(event, ProgressEvent::Failed { .. })),
        "extract + delete manifest task should finish without failures: {:?}",
        result.events
    );
    assert_eq!(
        std::fs::read_to_string(install_dir.join("payload.txt")).unwrap(),
        "updated payload"
    );
    assert_eq!(
        std::fs::read(install_dir.join("Arknights_Data/StreamingAssets/AB/Windows/ui/direct.ab"))
            .unwrap(),
        b"patched bytes"
    );
    assert!(!install_dir
        .join("Endfield_Data/Plugins/x86_64/libHAPI.dll")
        .exists());
    assert!(!install_dir.join("delete_files.txt").exists());
    assert!(!install_dir.join("patch.json").exists());
    assert!(!install_dir.join("vfs_files").exists());
}

use super::*;

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

    write_file(&linked, b"after".to_vec()).unwrap();

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
    let expected_md5 = griffr_core::to_hex(&Md5::digest(b"same-bytes"));

    let tasks = vec![Task::ensure_file(FileEnsureTask {
        dest: target.clone(),
        logical_path: "target.bin".to_string(),
        expected_hash: crate::ContentHash::from(expected_md5),
        expected_size: 10,
        source_candidates: vec![source.clone()],
        download_url: None,
        allow_copy_fallback: false,
        copy_only: false,
        prefer_reuse: true,
        retry_count: 0,
        transfer_class: TransferClass::General,
        archive_repair: None,
    })];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();
    assert!(
        result.outcomes.iter().any(|event| matches!(
            event,
            TaskOutcome::Committed { proof }
                if proof.logical_path() == "target.bin"
                    && proof.source() == crate::ArtifactSource::ReuseHardlink
        )),
        "expected committed artifact proof after relink"
    );
}

#[test]
fn relink_mode_keeps_valid_destination_when_no_source_can_be_reused() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("target.bin");
    std::fs::write(&target, b"destination-only").unwrap();
    let expected_md5 = griffr_core::to_hex(&Md5::digest(b"destination-only"));

    let tasks = vec![Task::ensure_file(FileEnsureTask {
        dest: target,
        logical_path: "target.bin".to_string(),
        expected_hash: crate::ContentHash::from(expected_md5),
        expected_size: 16,
        source_candidates: vec![tmp.path().join("missing-source.bin")],
        download_url: Some("http://127.0.0.1:1/must-not-download".to_string()),
        allow_copy_fallback: false,
        copy_only: false,
        prefer_reuse: true,
        retry_count: 0,
        transfer_class: TransferClass::General,
        archive_repair: None,
    })];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();
    assert!(result.outcomes.iter().any(|event| matches!(
        event,
        TaskOutcome::Verified {
            path,
            ok: true,
            issue: None,
        } if path == "target.bin"
    )));
    assert!(
        !result
            .outcomes
            .iter()
            .any(|event| matches!(event, TaskOutcome::Downloaded { .. })),
        "a valid destination must be the terminal fallback before network download"
    );
}

#[test]
fn ready_partial_is_saved_without_network_request() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("done.chk");
    let part = make_partial_download_path(&dest).unwrap();
    let payload = b"already finished partial download";
    std::fs::write(&part, payload).unwrap();
    let expected_md5 = griffr_core::to_hex(&Md5::digest(payload));

    let len = do_download(
        "Mozilla/5.0",
        "http://127.0.0.1:1/must-not-be-requested",
        &dest,
        &expected_md5,
        Some(payload.len() as u64),
        DEFAULT_PROGRESS_BUFFER_BYTES,
        None::<fn(crate::task_pool::download::DownloadProgress)>,
    )
    .unwrap();

    assert_eq!(len, payload.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(!part.exists());
}

fn do_download(
    user_agent: &str,
    url: &str,
    dest: &std::path::Path,
    expected_md5: &str,
    expected_size: Option<u64>,
    progress_buffer_bytes: usize,
    on_progress: Option<impl Fn(crate::task_pool::download::DownloadProgress) + Send + 'static>,
) -> crate::error::Result<u64> {
    use crate::task_pool::download::{do_prepared_download, prepare_download, DownloadPreparation};

    let expected_hash = crate::ContentHash::from(expected_md5);
    match prepare_download(dest, "test.bin", &expected_hash, expected_size)? {
        DownloadPreparation::Done(proof) => Ok(proof.observed_size()),
        DownloadPreparation::Resume(resume) => {
            let runtime =
                compio::runtime::Runtime::new().map_err(|error| crate::error::Error::Message {
                    context: "Task pool error: ",
                    detail: format!("failed to create async download test runtime: {error}"),
                })?;
            runtime
                .block_on(do_prepared_download(
                    user_agent,
                    url,
                    dest,
                    "test.bin",
                    &expected_hash,
                    expected_size,
                    resume,
                    progress_buffer_bytes,
                    on_progress,
                ))
                .map(|proof| proof.observed_size())
        }
    }
}

#[test]
fn bounded_download_writer_batches_small_body_chunks() {
    use compio::bytes::Bytes;
    use futures_util::stream;
    use std::time::Duration;

    let payload = (0..(2 * 1024 * 1024 + 17_321))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let chunks = payload
        .chunks(3079)
        .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
        .collect::<Vec<_>>();
    let expected_md5 = griffr_core::to_hex(&Md5::digest(&payload));
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("streamed.bin");
    let runtime = compio::runtime::Runtime::new().unwrap();
    let mut hasher = Md5::new();
    let mut progress = Vec::new();

    let written = runtime.block_on(async {
        let file = compio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&dest)
            .await
            .unwrap();
        let (file, written) = crate::task_pool::download_write::write_http_body(
            stream::iter(chunks),
            file,
            &dest,
            "test body",
            0,
            Duration::from_secs(5),
            |chunk| md5::Digest::update(&mut hasher, chunk),
            |written| progress.push(written),
        )
        .await
        .unwrap();
        file.close().await.unwrap();
        written
    });

    assert_eq!(written, payload.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert_eq!(
        griffr_core::to_hex(&md5::Digest::finalize(hasher)),
        expected_md5
    );
    assert_eq!(progress.last().copied(), Some(written));
}

#[test]
fn bounded_download_writer_preserves_resumed_prefix() {
    use compio::bytes::Bytes;
    use futures_util::stream;
    use std::time::Duration;

    let prefix = b"already-downloaded-prefix".to_vec();
    let suffix = (0..(1024 * 1024 + 8193))
        .map(|index| (index % 239) as u8)
        .collect::<Vec<_>>();
    let chunks = suffix
        .chunks(4093)
        .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
        .collect::<Vec<_>>();
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("resumed.bin");
    std::fs::write(&dest, &prefix).unwrap();
    let runtime = compio::runtime::Runtime::new().unwrap();
    let mut progress = Vec::new();

    let written = runtime.block_on(async {
        let file = compio::fs::OpenOptions::new()
            .write(true)
            .truncate(false)
            .open(&dest)
            .await
            .unwrap();
        let (file, written) = crate::task_pool::download_write::write_http_body(
            stream::iter(chunks),
            file,
            &dest,
            "resumed body",
            prefix.len() as u64,
            Duration::from_secs(5),
            |_| {},
            |written| progress.push(written),
        )
        .await
        .unwrap();
        file.close().await.unwrap();
        written
    });

    let mut expected = prefix;
    expected.extend_from_slice(&suffix);
    assert_eq!(written, expected.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), expected);
    assert_eq!(progress.last().copied(), Some(written));
}

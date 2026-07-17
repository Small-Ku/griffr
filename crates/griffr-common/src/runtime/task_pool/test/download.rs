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

    let tasks = vec![Task::ensure_file(FileEnsureTask {
        dest: target.clone(),
        logical_path: "target.bin".to_string(),
        expected_md5,
        expected_size: 10,
        source_candidates: vec![source.clone()],
        download_url: None,
        allow_copy_fallback: false,
        prefer_reuse: true,
        retry_count: 0,
        transfer_class: TransferClass::General,
    })];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();
    assert!(
        result
            .outcomes
            .iter()
            .any(|e| matches!(e, TaskOutcome::Hardlinked { .. })),
        "expected hardlink event when prefer_reuse is enabled"
    );
    assert!(
        result.outcomes.iter().any(|e| matches!(
            e,
            TaskOutcome::Verified {
                ok: true,
                issue: None,
                ..
            }
        )),
        "expected verify success after relink"
    );
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

    let (base, range_hits, total_hits, stop) = start_range_http_channel("/blob", payload.clone());
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
fn completed_partial_is_committed_without_network_request() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("complete.chk");
    let part = make_partial_download_path(&dest).unwrap();
    let payload = b"already complete partial download";
    std::fs::write(&part, payload).unwrap();
    let expected_md5 = format!("{:x}", Md5::digest(payload));

    let dispatcher = Dispatcher::builder()
        .worker_threads(NonZeroUsize::new(2).unwrap())
        .build()
        .expect("dispatcher should build");
    let len = do_download(
        Some(&dispatcher),
        "Mozilla/5.0",
        "http://127.0.0.1:1/must-not-be-requested",
        &dest,
        &expected_md5,
        Some(payload.len() as u64),
        DEFAULT_PROGRESS_BUFFER_BYTES,
        None::<fn(u64)>,
    )
    .unwrap();

    assert_eq!(len, payload.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(!part.exists());
}

fn do_download(
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    url: &str,
    dest: &std::path::Path,
    expected_md5: &str,
    expected_size: Option<u64>,
    progress_buffer_bytes: usize,
    on_progress: Option<impl Fn(u64) + Send + 'static>,
) -> crate::error::Result<u64> {
    use crate::runtime::task_pool::download::{
        do_prepared_download, prepare_download, DownloadPreparation,
    };

    match prepare_download(io_dispatcher, dest, expected_md5, expected_size)? {
        DownloadPreparation::Complete(bytes) => Ok(bytes),
        DownloadPreparation::Ready(resume) => do_prepared_download(
            io_dispatcher,
            user_agent,
            url,
            dest,
            expected_md5,
            expected_size,
            resume,
            progress_buffer_bytes,
            on_progress,
        ),
    }
}

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
    let expected_md5 = crate::to_hex(&Md5::digest(b"same-bytes"));

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
fn relink_mode_keeps_valid_destination_when_no_source_can_be_reused() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("target.bin");
    std::fs::write(&target, b"destination-only").unwrap();
    let expected_md5 = crate::to_hex(&Md5::digest(b"destination-only"));

    let tasks = vec![Task::ensure_file(FileEnsureTask {
        dest: target,
        logical_path: "target.bin".to_string(),
        expected_md5,
        expected_size: 16,
        source_candidates: vec![tmp.path().join("missing-source.bin")],
        download_url: Some("http://127.0.0.1:1/must-not-download".to_string()),
        allow_copy_fallback: false,
        prefer_reuse: true,
        retry_count: 0,
        transfer_class: TransferClass::General,
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
    let expected_md5 = crate::to_hex(&Md5::digest(payload));

    let len = do_download(
        "Mozilla/5.0",
        "http://127.0.0.1:1/must-not-be-requested",
        &dest,
        &expected_md5,
        Some(payload.len() as u64),
        DEFAULT_PROGRESS_BUFFER_BYTES,
        None::<fn(crate::runtime::task_pool::download::DownloadProgress)>,
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
    on_progress: Option<
        impl Fn(crate::runtime::task_pool::download::DownloadProgress) + Send + 'static,
    >,
) -> crate::error::Result<u64> {
    use crate::runtime::task_pool::download::{
        do_prepared_download, prepare_download, DownloadPreparation,
    };

    match prepare_download(dest, expected_md5, expected_size)? {
        DownloadPreparation::Done(bytes) => Ok(bytes),
        DownloadPreparation::Resume(resume) => {
            let runtime =
                compio::runtime::Runtime::new().map_err(|error| crate::error::Error::Message {
                    context: "Task pool error: ",
                    detail: format!("failed to create async download test runtime: {error}"),
                })?;
            runtime.block_on(do_prepared_download(
                user_agent,
                url,
                dest,
                expected_md5,
                expected_size,
                resume,
                progress_buffer_bytes,
                on_progress,
            ))
        }
    }
}

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};

use super::*;
use crate::runtime::task_pool::fs_ops::{
    commit_observed_artifact, verify_artifact, write_atomic_bytes,
};
use crate::runtime::{ArtifactDigest, ArtifactExpectation, ArtifactSource};

fn state(
    kind: InstallChangeKind,
    source: InstallChangeSource,
    from: Option<&str>,
    to: &str,
) -> InstallChangeState {
    InstallChangeState::new(
        kind,
        source,
        "endfield",
        "cn",
        "1",
        "1",
        from.map(str::to_string),
        to,
        Some("0123456789abcdef0123456789abcdef".to_string()),
        vec!["fedcba9876543210fedcba9876543210".to_string()],
        true,
    )
}

#[test]
fn private_namespace_match_is_case_and_separator_insensitive() {
    assert!(is_install_change_path(Path::new(
        ".GRIFFR-CHANGE\\STATE.JSON"
    )));
    assert!(is_install_change_path(Path::new(
        "./.griffr-change/cache.bin"
    )));
    assert!(is_install_change_path(Path::new(".griffr-change")));
    assert!(!is_install_change_path(Path::new(
        "data/.griffr-change/state.json"
    )));
    assert!(!is_install_change_path(Path::new(
        ".griffr-change-other/state.json"
    )));
}

#[test]
fn start_writes_and_same_change_resumes() {
    let temp = tempfile::tempdir().unwrap();
    let requested = state(
        InstallChangeKind::Update,
        InstallChangeSource::PatchArchive,
        Some("1.0"),
        "1.1",
    );

    assert_eq!(
        start_install_change(temp.path(), &requested).unwrap(),
        InstallChangeStart::New
    );
    assert_eq!(
        start_install_change(temp.path(), &requested).unwrap(),
        InstallChangeStart::Resume
    );
    assert!(InstallChangeState::state_path(temp.path()).is_file());
}

#[test]
fn forward_update_can_advance_from_previous_target() {
    let temp = tempfile::tempdir().unwrap();
    let first = state(
        InstallChangeKind::Update,
        InstallChangeSource::PatchArchive,
        Some("1.0"),
        "1.1",
    );
    let next = state(
        InstallChangeKind::Update,
        InstallChangeSource::FullArchive,
        Some("1.1"),
        "1.2",
    );
    start_install_change(temp.path(), &first).unwrap();

    assert_eq!(
        start_install_change(temp.path(), &next).unwrap(),
        InstallChangeStart::Advance
    );
    assert_eq!(read_install_change(temp.path()).unwrap(), Some(next));
}

#[test]
fn same_update_edge_can_switch_source() {
    let temp = tempfile::tempdir().unwrap();
    let patch = state(
        InstallChangeKind::Update,
        InstallChangeSource::PatchArchive,
        Some("1.0"),
        "1.1",
    );
    let full = state(
        InstallChangeKind::Update,
        InstallChangeSource::FullArchive,
        Some("1.0"),
        "1.1",
    );
    start_install_change(temp.path(), &patch).unwrap();

    assert_eq!(
        start_install_change(temp.path(), &full).unwrap(),
        InstallChangeStart::Advance
    );
    assert_eq!(read_install_change(temp.path()).unwrap(), Some(full));
}

#[test]
fn same_install_target_can_switch_from_reuse_to_archive() {
    let temp = tempfile::tempdir().unwrap();
    let reuse = state(
        InstallChangeKind::Install,
        InstallChangeSource::Reuse,
        None,
        "1.1",
    );
    let archive = state(
        InstallChangeKind::Install,
        InstallChangeSource::FullArchive,
        None,
        "1.1",
    );
    start_install_change(temp.path(), &reuse).unwrap();

    assert_eq!(
        start_install_change(temp.path(), &archive).unwrap(),
        InstallChangeStart::Advance
    );
}

#[test]
fn mixed_update_can_advance_directly_to_a_newer_live_target() {
    let temp = tempfile::tempdir().unwrap();
    let interrupted = state(
        InstallChangeKind::Update,
        InstallChangeSource::PatchArchive,
        Some("1.0"),
        "1.1",
    );
    let live = state(
        InstallChangeKind::Update,
        InstallChangeSource::FullArchive,
        Some("1.0"),
        "1.2",
    );
    start_install_change(temp.path(), &interrupted).unwrap();

    assert_eq!(
        start_install_change(temp.path(), &live).unwrap(),
        InstallChangeStart::Advance
    );
    assert_eq!(read_install_change(temp.path()).unwrap(), Some(live));
}

#[test]
fn unfinished_repair_can_advance_to_a_new_release() {
    let temp = tempfile::tempdir().unwrap();
    let repair = state(
        InstallChangeKind::Repair,
        InstallChangeSource::Repair,
        Some("1.1"),
        "1.1",
    );
    let live = state(
        InstallChangeKind::Update,
        InstallChangeSource::Repair,
        Some("1.1"),
        "1.2",
    );
    start_install_change(temp.path(), &repair).unwrap();

    assert_eq!(
        start_install_change(temp.path(), &live).unwrap(),
        InstallChangeStart::Advance
    );
}

#[test]
fn same_repair_can_change_vfs_scope_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    let with_vfs = state(
        InstallChangeKind::Repair,
        InstallChangeSource::Repair,
        Some("1.1"),
        "1.1",
    );
    let mut without_vfs = with_vfs.clone();
    without_vfs.sync_vfs = false;
    start_install_change(temp.path(), &with_vfs).unwrap();

    assert_eq!(
        start_install_change(temp.path(), &without_vfs).unwrap(),
        InstallChangeStart::Advance
    );
    assert_eq!(read_install_change(temp.path()).unwrap(), Some(without_vfs));
}

#[test]
fn release_identity_uses_target_and_manifest_digest() {
    let requested = state(
        InstallChangeKind::Update,
        InstallChangeSource::PatchArchive,
        Some("1.0"),
        "1.1",
    );

    assert!(requested.matches_release("1.1", Some("0123456789ABCDEF0123456789ABCDEF")));
    assert!(!requested.matches_release("1.2", Some("0123456789abcdef0123456789abcdef")));
    assert!(!requested.matches_release("1.1", Some("fedcba9876543210fedcba9876543210")));
}

#[test]
fn unrelated_pending_change_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let update = state(
        InstallChangeKind::Update,
        InstallChangeSource::PatchArchive,
        Some("1.0"),
        "1.1",
    );
    let repair = state(
        InstallChangeKind::Repair,
        InstallChangeSource::Repair,
        Some("1.0"),
        "1.0",
    );
    start_install_change(temp.path(), &update).unwrap();

    let error = start_install_change(temp.path(), &repair).unwrap_err();
    assert!(error.to_string().contains("conflicts"));
    assert_eq!(read_install_change(temp.path()).unwrap(), Some(update));
}

#[test]
fn constructor_normalizes_digest_identity() {
    let state = InstallChangeState::new(
        InstallChangeKind::Install,
        InstallChangeSource::FullArchive,
        "endfield",
        "cn",
        "1",
        "1",
        None,
        "1.1",
        Some("ABCDEF0123456789ABCDEF0123456789".to_string()),
        vec!["FEDCBA9876543210FEDCBA9876543210".to_string()],
        true,
    );

    assert_eq!(
        state.game_files_md5.as_deref(),
        Some("abcdef0123456789abcdef0123456789")
    );
    assert_eq!(state.payload_md5s, vec!["fedcba9876543210fedcba9876543210"]);
}

#[test]
fn finish_removes_only_matching_state() {
    let temp = tempfile::tempdir().unwrap();
    let requested = state(
        InstallChangeKind::Install,
        InstallChangeSource::FullArchive,
        None,
        "1.1",
    );
    start_install_change(temp.path(), &requested).unwrap();

    let wrong = state(
        InstallChangeKind::Install,
        InstallChangeSource::FullArchive,
        None,
        "1.2",
    );
    assert!(finish_install_change(temp.path(), &wrong).is_err());
    assert!(read_install_change(temp.path()).unwrap().is_some());

    finish_install_change(temp.path(), &requested).unwrap();
    assert!(read_install_change(temp.path()).unwrap().is_none());
}

#[test]
fn marker_remains_when_work_stops_before_finish() {
    let temp = tempfile::tempdir().unwrap();
    let requested = state(
        InstallChangeKind::Repair,
        InstallChangeSource::Repair,
        Some("1.1"),
        "1.1",
    );
    start_install_change(temp.path(), &requested).unwrap();
    std::fs::write(temp.path().join("partial-output"), b"partial").unwrap();

    assert_eq!(read_install_change(temp.path()).unwrap(), Some(requested));
}

#[test]
fn unfinished_marker_blocks_launch_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let requested = state(
        InstallChangeKind::Update,
        InstallChangeSource::FullArchive,
        Some("1.0"),
        "1.1",
    );
    start_install_change(temp.path(), &requested).unwrap();

    let error = ensure_install_ready(temp.path()).unwrap_err();
    assert!(error.to_string().contains("launch is blocked"));

    finish_install_change(temp.path(), &requested).unwrap();
    ensure_install_ready(temp.path()).unwrap();
}

#[test]
fn marker_survives_a_crash_after_version_metadata_commit() {
    let temp = tempfile::tempdir().unwrap();
    let requested = state(
        InstallChangeKind::Update,
        InstallChangeSource::FullArchive,
        Some("1.0"),
        "1.1",
    );
    start_install_change(temp.path(), &requested).unwrap();

    // This models the last crash window: config.ini has advertised the
    // target, but the command has not yet removed the operation marker.
    std::fs::write(temp.path().join("config.ini"), b"version=1.1\n").unwrap();

    assert_eq!(read_install_change(temp.path()).unwrap(), Some(requested));
    assert!(ensure_install_ready(temp.path()).is_err());
}

#[test]
fn atomic_marker_write_leaves_only_the_state_file() {
    let temp = tempfile::tempdir().unwrap();
    let requested = state(
        InstallChangeKind::Install,
        InstallChangeSource::FullArchive,
        None,
        "1.1",
    );
    start_install_change(temp.path(), &requested).unwrap();

    let entries = std::fs::read_dir(temp.path().join(INSTALL_CHANGE_DIR))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(entries, vec![INSTALL_CHANGE_STATE_NAME.to_string()]);
}

const PROCESS_KILL_SCENARIO_ENV: &str = "GRIFFR_TEST_PROCESS_KILL_SCENARIO";
const PROCESS_KILL_ROOT_ENV: &str = "GRIFFR_TEST_PROCESS_KILL_ROOT";
const CHECKPOINT_ENV: &str = "GRIFFR_TEST_CHECKPOINT";
const CHECKPOINT_READY_ENV: &str = "GRIFFR_TEST_CHECKPOINT_READY";

fn spawn_process_kill_worker(root: &Path, scenario: &str, checkpoint: &str) -> (Child, PathBuf) {
    let ready_path = root.join(format!("{scenario}.ready"));
    let log_path = root.join(format!("{scenario}.log"));
    let log = File::create(&log_path).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--ignored")
        .arg("--nocapture")
        .arg("install_change_process_kill_worker")
        .env(PROCESS_KILL_SCENARIO_ENV, scenario)
        .env(PROCESS_KILL_ROOT_ENV, root)
        .env(CHECKPOINT_ENV, checkpoint)
        .env(CHECKPOINT_READY_ENV, &ready_path)
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    (child, ready_path)
}

fn kill_at_checkpoint(root: &Path, scenario: &str, checkpoint: &str) {
    let (mut child, ready_path) = spawn_process_kill_worker(root, scenario, checkpoint);
    let started = Instant::now();
    loop {
        if ready_path.is_file() {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let log =
                std::fs::read_to_string(root.join(format!("{scenario}.log"))).unwrap_or_default();
            panic!("process-kill worker exited early with {status}:\n{log}");
        }
        if started.elapsed() >= Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            let log =
                std::fs::read_to_string(root.join(format!("{scenario}.log"))).unwrap_or_default();
            panic!("process-kill worker did not reach checkpoint {checkpoint}:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success(), "killed worker unexpectedly succeeded");
}

fn setup_pending_update(root: &Path) -> InstallChangeState {
    let requested = state(
        InstallChangeKind::Update,
        InstallChangeSource::FullArchive,
        Some("1.0"),
        "1.1",
    );
    start_install_change(root, &requested).unwrap();
    requested
}

fn artifact_expectation() -> (ArtifactExpectation, ArtifactDigest) {
    let payload = b"new-data";
    let md5 = crate::to_hex(&Md5::digest(payload));
    (
        ArtifactExpectation::new("payload.bin", &md5, Some(payload.len() as u64)),
        ArtifactDigest::new(payload.len() as u64, md5),
    )
}

#[test]
fn process_kill_during_first_marker_write_leaves_install_unmarked() {
    let temp = tempfile::tempdir().unwrap();

    kill_at_checkpoint(temp.path(), "marker_new", "atomic_write.after_sync");

    assert!(read_install_change(temp.path()).unwrap().is_none());
    let requested = state(
        InstallChangeKind::Install,
        InstallChangeSource::FullArchive,
        None,
        "1.1",
    );
    assert_eq!(
        start_install_change(temp.path(), &requested).unwrap(),
        InstallChangeStart::New
    );
    finish_install_change(temp.path(), &requested).unwrap();
    assert!(!temp.path().join(INSTALL_CHANGE_DIR).exists());
}

#[test]
fn process_kill_during_marker_replace_keeps_previous_state() {
    let temp = tempfile::tempdir().unwrap();
    let current = state(
        InstallChangeKind::Update,
        InstallChangeSource::PatchArchive,
        Some("1.0"),
        "1.1",
    );
    start_install_change(temp.path(), &current).unwrap();

    kill_at_checkpoint(temp.path(), "marker_advance", "atomic_write.after_sync");

    assert_eq!(read_install_change(temp.path()).unwrap(), Some(current));
    let next = state(
        InstallChangeKind::Update,
        InstallChangeSource::FullArchive,
        Some("1.1"),
        "1.2",
    );
    assert_eq!(
        start_install_change(temp.path(), &next).unwrap(),
        InstallChangeStart::Advance
    );
    assert_eq!(
        read_install_change(temp.path()).unwrap(),
        Some(next.clone())
    );
    finish_install_change(temp.path(), &next).unwrap();
    assert!(!temp.path().join(INSTALL_CHANGE_DIR).exists());
}

#[test]
fn process_kill_before_artifact_replace_keeps_old_destination() {
    let temp = tempfile::tempdir().unwrap();
    let requested = setup_pending_update(temp.path());
    std::fs::write(temp.path().join("payload.bin"), b"old-data").unwrap();

    kill_at_checkpoint(temp.path(), "artifact_commit", "artifact.before_replace");

    assert_eq!(
        std::fs::read(temp.path().join("payload.bin")).unwrap(),
        b"old-data"
    );
    assert_eq!(
        std::fs::read(temp.path().join("payload.stage")).unwrap(),
        b"new-data"
    );
    assert_eq!(read_install_change(temp.path()).unwrap(), Some(requested));
}

#[test]
fn process_kill_after_artifact_replace_keeps_verified_new_destination() {
    let temp = tempfile::tempdir().unwrap();
    let requested = setup_pending_update(temp.path());
    std::fs::write(temp.path().join("payload.bin"), b"old-data").unwrap();

    kill_at_checkpoint(temp.path(), "artifact_commit", "artifact.after_replace");

    assert_eq!(
        std::fs::read(temp.path().join("payload.bin")).unwrap(),
        b"new-data"
    );
    assert!(!temp.path().join("payload.stage").exists());
    let (expectation, _) = artifact_expectation();
    let proof = verify_artifact(
        &temp.path().join("payload.bin"),
        &expectation,
        ArtifactSource::Existing,
    )
    .unwrap();
    assert!(proof.is_current());
    assert_eq!(read_install_change(temp.path()).unwrap(), Some(requested));
}

#[test]
fn process_kill_after_config_commit_still_blocks_launch() {
    let temp = tempfile::tempdir().unwrap();
    let requested = setup_pending_update(temp.path());

    kill_at_checkpoint(temp.path(), "config_before_finish", "change.before_finish");

    assert_eq!(
        std::fs::read(temp.path().join("config.ini")).unwrap(),
        b"version=1.1\n"
    );
    assert_eq!(
        read_install_change(temp.path()).unwrap(),
        Some(requested.clone())
    );
    assert!(ensure_install_ready(temp.path()).is_err());

    finish_install_change(temp.path(), &requested).unwrap();
    ensure_install_ready(temp.path()).unwrap();
}

#[test]
fn process_kill_after_marker_removal_leaves_install_ready() {
    let temp = tempfile::tempdir().unwrap();
    setup_pending_update(temp.path());

    kill_at_checkpoint(temp.path(), "finish_change", "change.after_finish");

    assert!(read_install_change(temp.path()).unwrap().is_none());
    ensure_install_ready(temp.path()).unwrap();
}

#[test]
#[ignore = "spawned by the process-kill recovery tests"]
fn install_change_process_kill_worker() {
    let Some(scenario) = std::env::var(PROCESS_KILL_SCENARIO_ENV).ok() else {
        return;
    };
    let root = PathBuf::from(
        std::env::var_os(PROCESS_KILL_ROOT_ENV).expect("process-kill worker root is missing"),
    );

    match scenario.as_str() {
        "marker_new" => {
            let requested = state(
                InstallChangeKind::Install,
                InstallChangeSource::FullArchive,
                None,
                "1.1",
            );
            start_install_change(&root, &requested).unwrap();
        }
        "marker_advance" => {
            let next = state(
                InstallChangeKind::Update,
                InstallChangeSource::FullArchive,
                Some("1.1"),
                "1.2",
            );
            start_install_change(&root, &next).unwrap();
        }
        "artifact_commit" => {
            let source = root.join("payload.stage");
            let destination = root.join("payload.bin");
            std::fs::write(&source, b"new-data").unwrap();
            let (expectation, digest) = artifact_expectation();
            commit_observed_artifact(
                &source,
                &destination,
                &expectation,
                ArtifactSource::Archive,
                &digest,
            )
            .unwrap();
        }
        "config_before_finish" => {
            write_atomic_bytes(&root.join("config.ini"), b"version=1.1\n").unwrap();
            crate::runtime::test_checkpoint::hit("change.before_finish");
            let requested = state(
                InstallChangeKind::Update,
                InstallChangeSource::FullArchive,
                Some("1.0"),
                "1.1",
            );
            finish_install_change(&root, &requested).unwrap();
        }
        "finish_change" => {
            let requested = state(
                InstallChangeKind::Update,
                InstallChangeSource::FullArchive,
                Some("1.0"),
                "1.1",
            );
            finish_install_change(&root, &requested).unwrap();
            crate::runtime::test_checkpoint::hit("change.after_finish");
        }
        other => panic!("unknown process-kill scenario {other}"),
    }
}

use super::*;
use crate::runtime::task_pool::verify::{file_md5, VerifiedArtifactCache};
use crate::runtime::{griffr_patch_path, PlannedPatchEntry, PlannedPatchSource};
use md5::Digest;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn plan(
    install_root: &Path,
    stage_root: &Path,
    entries: Vec<PlannedPatchEntry>,
    delete_paths: Vec<PathBuf>,
    deferred_paths: Vec<PathBuf>,
) -> PatchPlan {
    PatchPlan {
        schema_version: PatchPlan::SCHEMA_VERSION,
        install_root: install_root.to_path_buf(),
        stage_root: stage_root.to_path_buf(),
        vfs_base_path: PathBuf::from("Game_Data/StreamingAssets/VFS"),
        vfs_destination: install_root.join("Game_Data/StreamingAssets/VFS"),
        work_dir: None,
        entries,
        delete_paths,
        deferred_paths,
    }
}

#[test]
fn patch_apply_defers_version_marker_and_preserves_final_output() {
    let temp = tempdir().unwrap();
    let install_root = temp.path().join("install");
    let stage_root = temp.path().join("stage");
    let output = install_root.join("Game_Data/StreamingAssets/VFS/final.bin");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&stage_root).unwrap();
    std::fs::write(&output, b"final").unwrap();
    std::fs::write(stage_root.join("config.ini"), b"version=2").unwrap();
    std::fs::write(stage_root.join("top-level.bin"), b"replacement").unwrap();
    std::fs::write(install_root.join("config.ini"), b"version=1").unwrap();

    let plan = plan(
        &install_root,
        &stage_root,
        vec![PlannedPatchEntry {
            name: "final.bin".to_string(),
            destination: output.clone(),
            expected_md5: file_md5(&output).unwrap(),
            expected_size: 5,
            source: PlannedPatchSource::AlreadyPresent,
        }],
        Vec::new(),
        vec![PathBuf::from("config.ini")],
    );

    let proofs = run_patch_apply(
        &plan,
        None,
        None,
        None,
        None,
        &VerifiedArtifactCache::default(),
    )
    .unwrap();

    assert_eq!(proofs.len(), 1);
    assert_eq!(
        proofs[0].logical_path(),
        "Game_Data/StreamingAssets/VFS/final.bin"
    );
    assert_eq!(proofs[0].source(), crate::runtime::ArtifactSource::Existing);
    assert_eq!(std::fs::read(&output).unwrap(), b"final");
    assert_eq!(
        std::fs::read(install_root.join("top-level.bin")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        std::fs::read(install_root.join("config.ini")).unwrap(),
        b"version=2"
    );
    assert!(!stage_root.exists());
    assert!(!griffr_patch_path(&install_root).exists());
}

#[test]
fn plan_rejects_delete_manifest_conflict_with_output() {
    let temp = tempdir().unwrap();
    let install_root = temp.path().join("install");
    let stage_root = temp.path().join("stage");
    let output = install_root.join("Game_Data/StreamingAssets/VFS/final.bin");
    let plan = plan(
        &install_root,
        &stage_root,
        vec![PlannedPatchEntry {
            name: "final.bin".to_string(),
            destination: output,
            expected_md5: "a".to_string(),
            expected_size: 1,
            source: PlannedPatchSource::AlreadyPresent,
        }],
        vec![PathBuf::from("Game_Data/StreamingAssets/VFS/final.bin")],
        Vec::new(),
    );

    assert!(plan.validate().is_err());
}

#[test]
fn base_is_released_only_after_last_consumer() {
    let temp = tempdir().unwrap();
    let install_root = temp.path().join("install");
    let stage_root = temp.path().join("stage");
    let base = install_root.join("Game_Data/StreamingAssets/VFS/old.bin");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(&base, b"old").unwrap();
    let plan = plan(
        &install_root,
        &stage_root,
        Vec::new(),
        vec![PathBuf::from("Game_Data/StreamingAssets/VFS/old.bin")],
        Vec::new(),
    );
    let mut remaining = BTreeMap::from([(base.clone(), 2usize)]);
    let deletes = plan.delete_paths.iter().cloned().collect();
    let outputs = BTreeSet::new();

    release_base_if_unused(&plan, &base, &mut remaining, &deletes, &outputs).unwrap();
    assert!(base.exists());
    release_base_if_unused(&plan, &base, &mut remaining, &deletes, &outputs).unwrap();
    assert!(!base.exists());
}

#[test]
fn application_revalidates_persisted_base_metadata() {
    let temp = tempdir().unwrap();
    let install_root = temp.path().join("install");
    let stage_root = temp.path().join("stage");
    let base = install_root.join("Game_Data/StreamingAssets/VFS/base.bin");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(&base, b"changed").unwrap();

    let entry = PlannedPatchEntry {
        name: "output.bin".to_string(),
        destination: install_root.join("Game_Data/StreamingAssets/VFS/output.bin"),
        expected_md5: "00000000000000000000000000000000".to_string(),
        expected_size: 1,
        source: PlannedPatchSource::Hdiff {
            base,
            payload: PathBuf::from("vfs_files/vfs_patch/output.patch"),
            base_md5: "11111111111111111111111111111111".to_string(),
            base_size: 7,
        },
    };
    let plan = plan(
        &install_root,
        &stage_root,
        vec![entry.clone()],
        Vec::new(),
        Vec::new(),
    );

    let error = apply_planned_entry(&plan, &entry, &VerifiedArtifactCache::default()).unwrap_err();
    assert!(error
        .to_string()
        .contains("failed verification before applying"));
}

#[test]
fn dependency_order_uses_logical_path_for_external_vfs() {
    let temp = tempdir().unwrap();
    let install_root = temp.path().join("install");
    let stage_root = temp.path().join("stage");
    let external = temp.path().join("external");
    let logical_base = install_root.join("Game_Data/StreamingAssets/VFS/intermediate.bin");
    let consumer = PlannedPatchEntry {
        name: "final.bin".to_string(),
        destination: external.join("final.bin"),
        expected_md5: "a".to_string(),
        expected_size: 1,
        source: PlannedPatchSource::Hdiff {
            base: logical_base,
            payload: PathBuf::from("vfs_files/vfs_patch/final.patch"),
            base_md5: "c".to_string(),
            base_size: 1,
        },
    };
    let writer = PlannedPatchEntry {
        name: "intermediate.bin".to_string(),
        destination: external.join("intermediate.bin"),
        expected_md5: "b".to_string(),
        expected_size: 1,
        source: PlannedPatchSource::Local {
            payload: PathBuf::from("vfs_files/files/intermediate.bin"),
        },
    };
    let mut plan = plan(
        &install_root,
        &stage_root,
        vec![writer, consumer],
        Vec::new(),
        Vec::new(),
    );
    plan.vfs_destination = external;

    let order = ordered_entries(&plan).unwrap();
    assert_eq!(order[0].name, "final.bin");
    assert_eq!(order[1].name, "intermediate.bin");
}

#[test]
fn corrupt_local_payload_does_not_replace_existing_destination() {
    let temp = tempdir().unwrap();
    let install_root = temp.path().join("install");
    let stage_root = temp.path().join("stage");
    let destination = install_root.join("Game_Data/StreamingAssets/VFS/output.bin");
    let payload = stage_root.join("vfs_files/files/output.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"old output").unwrap();
    std::fs::write(&payload, b"corrupt payload").unwrap();

    let entry = PlannedPatchEntry {
        name: "output.bin".to_string(),
        destination: destination.clone(),
        expected_md5: crate::to_hex(&md5::Md5::digest(b"expected payload")),
        expected_size: 16,
        source: PlannedPatchSource::Local {
            payload: PathBuf::from("vfs_files/files/output.bin"),
        },
    };
    let plan = plan(
        &install_root,
        &stage_root,
        vec![entry.clone()],
        Vec::new(),
        Vec::new(),
    );

    let error = apply_planned_entry(&plan, &entry, &VerifiedArtifactCache::default()).unwrap_err();

    assert!(error.to_string().contains("failed verification"));
    assert_eq!(std::fs::read(&destination).unwrap(), b"old output");
    assert!(payload.exists());
}

#[test]
fn missing_deferred_marker_blocks_final_commit() {
    let temp = tempdir().unwrap();
    let install_root = temp.path().join("install");
    let stage_root = temp.path().join("stage");
    std::fs::create_dir_all(&stage_root).unwrap();
    let plan = plan(
        &install_root,
        &stage_root,
        Vec::new(),
        Vec::new(),
        vec![PathBuf::from("config.ini")],
    );

    let error = commit_deferred_files(&plan).unwrap_err();

    assert!(error.to_string().contains("Missing deferred patch file"));
    assert!(!install_root.join("config.ini").exists());
}

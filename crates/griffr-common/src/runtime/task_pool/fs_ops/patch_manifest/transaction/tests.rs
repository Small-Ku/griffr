use super::*;
use crate::runtime::task_pool::verify::file_md5;
use crate::runtime::{PlannedPatchEntry, PlannedPatchSource, PATCH_TRANSACTION_DIR};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn plan(
    install_root: &Path,
    stage_root: &Path,
    entries: Vec<PlannedPatchEntry>,
    delete_paths: Vec<PathBuf>,
    deferred_paths: Vec<PathBuf>,
) -> PatchExecutionPlan {
    PatchExecutionPlan {
        schema_version: PatchExecutionPlan::SCHEMA_VERSION,
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
fn transaction_defers_version_marker_and_preserves_final_output() {
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

    execute_patch_transaction(&plan, None, None, None, None, 2).unwrap();

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
    assert!(!install_root.join(PATCH_TRANSACTION_DIR).exists());
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

    let error = apply_planned_entry(&plan, &entry).unwrap_err();
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

pub mod admin;
mod compat_fs;
mod file_allocation;
pub mod files;
mod install_plan;
mod integrity;
pub mod issues;
pub mod launcher;
mod launcher_metadata;
mod local_install;
mod patch_transaction;
mod paths;
mod progress;
pub mod task_pool;
mod update_plan;

pub use admin::{ensure_admin, is_running_as_admin, restart_as_admin};
pub use compat_fs::{
    collect_files_recursive, copy_dir_recursive, dir_size, directory_has_entries,
    list_files_with_extension, read_link, remove_dir_all, remove_empty_dirs_recursive, CopyStats,
};
pub(crate) use file_allocation::preallocate_file;

pub use files::reuse::{
    ensure_game_files_with_pool, inspect_reuse_installations, resolve_file_reuse_sources,
    FileEnsureSummary, FileReuseConfig, SourceInstallInput,
};
pub use files::vfs::{
    bootstrap_persistent_vfs_with_runner, download_vfs_resources, get_vfs_resource_info,
    plan_persistent_bootstrap_tasks, plan_vfs_tasks, VfsBootstrapConfig, VfsBootstrapPlan,
    VfsBootstrapResult, VfsBootstrapScope, VfsFilePlanOptions, VfsPlanOutcome, VfsTaskPlan,
    VfsUpdateOutcome, VfsUpdateResult,
};
pub use install_plan::required_install_bytes;
pub use integrity::{run_integrity_pool, IntegrityRunSummary, IntegritySelection};
pub use issues::{FileIssue, FileIssueKind};
pub use launcher::{GameProcess, Launcher};
pub use launcher_metadata::sync_launcher_metadata;
pub use local_install::{
    decrypt_config_ini, detect_local_install, resolve_install_path, resolve_named_path,
    LocalInstall, ParsedConfigIni,
};
pub(crate) use patch_transaction::build_patch_execution_plan_with_cache;
pub use patch_transaction::{
    available_space, classify_patch_recovery, preflight_patch_archives,
    read_patch_storage_topology, read_predownload_stage_metadata, write_predownload_stage_metadata,
    PatchApplyOptions, PatchExecutionPlan, PatchPreflightReport, PatchRecoveryState,
    PatchStorageTopology, PlannedPatchEntry, PlannedPatchSource, PredownloadStageMetadata,
    StagedArchivePart, PATCH_DEFERRED_DIR, PATCH_PLAN_NAME, PATCH_STORAGE_METADATA_NAME,
    PATCH_TRANSACTION_DIR, PREDOWNLOAD_STAGE_METADATA_NAME,
};
pub use paths::{
    build_cdn_file_url, files_base_url, is_launcher_metadata_path, launcher_metadata_url,
    logical_path_from_root, normalize_logical_path, persistent_path, resource_manifest_filename,
    resource_manifest_url, streaming_assets_path, vfs_path, ResourceManifestKind, CDN_FILES_DIR,
    CONFIG_INI_NAME, DELETE_FILES_MANIFEST_NAME, GAME_FILES_NAME, PACKAGE_FILES_NAME,
    PATCH_DIFF_STAGE_DIR, PATCH_FILES_STAGE_DIR, PATCH_MANIFEST_NAME, PATCH_STAGE_DIR,
    PERSISTENT_DIR, RESOURCE_GROUP_INITIAL, RESOURCE_GROUP_MAIN, STREAMING_ASSETS_DIR, VFS_DIR,
};
pub use progress::{
    PathAttemptKind, PathOutcome, PathOutcomeSummary, PathOutcomeTracker, PathReuseMethod,
    ProgressLane, ProgressPhase, ProgressReceiver, ProgressScope, ProgressSender, ProgressUnit,
    ProgressUpdate,
};
pub use update_plan::{
    resolve_staged_patch_recovery_dir, select_update_package, selected_archive_plan,
    staged_patch_request_version, ArchivePlanSummary, UpdatePackageKind,
};

mod metadata;
mod model;
mod persistence;
mod planning;
mod recovery;
mod space;
mod storage;

pub use metadata::{
    read_predownload_stage_metadata, write_predownload_stage_metadata, PredownloadStageMetadata,
    StagedArchivePart, PREDOWNLOAD_STAGE_METADATA_NAME,
};
pub use model::{
    PatchApplyOptions, PatchExecutionPlan, PatchPreflightReport, PlannedPatchEntry,
    PlannedPatchSource,
};
pub(crate) use persistence::{read_patch_execution_plan, write_patch_execution_plan};
pub(crate) use planning::build_patch_execution_plan;
pub use planning::preflight_patch_archives;
pub use recovery::{classify_patch_recovery, PatchRecoveryState};
pub use space::available_space;
pub(crate) use storage::write_patch_storage_topology;
pub use storage::{read_patch_storage_topology, PatchStorageTopology};

pub const PATCH_TRANSACTION_DIR: &str = ".griffr-patch";
pub const PATCH_DEFERRED_DIR: &str = "deferred";
pub const PATCH_PLAN_NAME: &str = "plan.json";
pub const PATCH_STORAGE_METADATA_NAME: &str = ".griffr-storage.json";

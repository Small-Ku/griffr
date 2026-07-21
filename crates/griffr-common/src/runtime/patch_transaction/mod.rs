mod build_plan;
mod dependency;
mod metadata;
mod plan;
mod plan_file;
mod recovery;
mod space;
mod space_use;
mod storage;

pub(crate) use dependency::{entry_dependency_indices, entry_wave_indices};

pub use build_plan::check_patch_archives;
pub(crate) use build_plan::{
    build_patch_plan_with_probe_cache, plan_patch_probes, PatchArtifactProbe, PatchProbePlan,
};
pub use metadata::{
    read_predownload_stage_metadata, write_predownload_stage_metadata, PredownloadStageMetadata,
    StagedArchivePart, PREDOWNLOAD_STAGE_METADATA_NAME,
};
pub use plan::{
    PatchApplyOptions, PatchCheckReport, PatchPlan, PlannedPatchEntry, PlannedPatchSource,
};
pub(crate) use plan_file::{read_patch_plan, write_patch_plan};
pub use recovery::{get_patch_recovery_state, PatchRecoveryState};
pub use space::available_space;
pub(crate) use storage::write_patch_storage_layout;
pub use storage::{read_patch_storage_layout, PatchStorageLayout};

pub const PATCH_TRANSACTION_DIR: &str = ".griffr-patch";
pub const PATCH_DEFERRED_DIR: &str = "deferred";
pub const PATCH_PLAN_NAME: &str = "plan.json";
pub const PATCH_STORAGE_METADATA_NAME: &str = ".griffr-storage.json";

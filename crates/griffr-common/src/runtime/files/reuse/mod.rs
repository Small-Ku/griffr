pub mod legacy;
pub mod materialize;
mod models;
pub mod plan;
pub mod types;

pub use legacy::{download_remaining_files, execute_reuse_plan, print_reuse_plan_summary};
pub use materialize::{apply_file_reuse_flow, materialize_game_files_with_pool};
pub use plan::plan_file_reuse;
pub use types::{
    DownloadFile, FileReuseConfig, MaterializeSummary, ReusableFile, ReuseOptions, ReusePlan,
    SourceChannel, SourceInstallInput,
};

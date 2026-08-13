mod remote_media;
mod remote_resources;
#[cfg(test)]
mod tests;
mod vfs_analysis;
mod vfs_snapshot;
mod vfs_snapshot_diff;
mod vfs_support;
mod yostar;

pub use remote_media::{api_get_media, fetch_media};
pub use remote_resources::{
    api_get_latest_game, api_get_latest_resources, fetch_file, fetch_game_files,
    list_resource_files,
};
pub use vfs_analysis::{snapshot_resource_state, vfs_diff};
pub use vfs_snapshot::{config_ini, detect, game_files, res_index};
pub use vfs_snapshot_diff::diff_resource_snapshots;
pub use yostar::{
    cdn as yostar_cdn, config as yostar_config, file_url as yostar_file_url,
    manifest as yostar_manifest,
};

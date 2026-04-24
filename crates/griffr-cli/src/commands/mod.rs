//! CLI command implementations

pub mod account;
pub mod debug;
pub mod info;
pub mod install;
pub mod launch;
mod local;
pub mod news;
pub mod uninstall;
pub mod update;
pub mod verify;

pub use account::{activate as account_activate, capture as account_capture};
pub use debug::{
    api_get_latest_game as debug_api_get_latest_game,
    api_get_latest_resources as debug_api_get_latest_resources,
    api_get_media as debug_api_get_media, config_ini as debug_config_ini, detect as debug_detect,
    fetch_file as debug_fetch_file, fetch_game_files as debug_fetch_game_files,
    fetch_media as debug_fetch_media, game_files as debug_game_files,
    list_resource_files as debug_list_resource_files,
};
pub use info::show as info_show;
pub use install::install;
pub use launch::launch;
pub use news::show as news_show;
pub use uninstall::uninstall;
pub use update::update;
pub use verify::verify;

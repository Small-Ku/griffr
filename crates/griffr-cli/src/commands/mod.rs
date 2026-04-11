//! CLI command implementations

pub mod debug;
pub mod info;
pub mod install;
pub mod launch;
mod local;
pub mod news;
pub mod uninstall;
pub mod update;
pub mod verify;

pub use debug::{
    config_ini as debug_config_ini, detect as debug_detect, fetch_file as debug_fetch_file,
    fetch_game_files as debug_fetch_game_files, game_files as debug_game_files,
};
pub use info::show as info_show;
pub use install::install;
pub use launch::launch;
pub use news::show as news_show;
pub use uninstall::uninstall;
pub use update::update;
pub use verify::verify;

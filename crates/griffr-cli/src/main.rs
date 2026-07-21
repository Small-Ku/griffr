#![allow(clippy::too_many_arguments, clippy::type_complexity)]

#[path = "main/cli.rs"]
mod cli;
mod commands;
#[path = "main/debug_cli.rs"]
mod debug_cli;
#[path = "main/entrypoint/mod.rs"]
mod entrypoint;
mod progress;
mod ui;

pub(crate) use cli::*;
pub(crate) use debug_cli::*;

#[compio::main]
async fn main() -> anyhow::Result<()> {
    entrypoint::run().await
}

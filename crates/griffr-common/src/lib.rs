//! Griffr Common Library
//!
//! Shared library for the Griffr game launcher.
//! Provides configuration, API clients, download management, and game handling.

pub mod api;
pub mod config;
pub mod download;
pub mod game;

pub use config::Config;

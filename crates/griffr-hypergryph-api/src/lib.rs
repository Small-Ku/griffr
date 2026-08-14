//! Hypergryph/Gryphline launcher protocol and HTTP client.

pub mod client;
pub mod crypto;
mod error;
mod paths;
pub mod protocol;
mod targets;
pub mod types;

#[cfg(test)]
mod integration_tests;

pub use client::ApiClient;
pub use error::{Error, Result};
pub use paths::*;
pub use targets::*;
pub use types::*;

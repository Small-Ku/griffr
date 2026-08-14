//! Hypergryph/Gryphline launcher protocol and HTTP client.

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "crypto")]
pub mod crypto;
mod error;
mod paths;
pub mod protocol;
mod targets;
pub mod types;

#[cfg(all(test, feature = "client"))]
mod integration_tests;

#[cfg(feature = "client")]
pub use client::ApiClient;
pub use error::{Error, Result};
pub use paths::*;
pub use targets::*;
pub use types::*;

//! YoStar launcher protocol and HTTP client.

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
mod error;
mod target;

#[cfg(feature = "client")]
pub use client::*;
#[cfg(feature = "client")]
pub use error::{Error, Result};
pub use target::*;

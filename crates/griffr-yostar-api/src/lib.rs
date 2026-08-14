//! YoStar launcher protocol and HTTP client.

mod client;
mod error;
mod target;

pub use client::*;
pub use error::{Error, Result};
pub use target::*;

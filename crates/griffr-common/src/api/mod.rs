//! Hypergryph API client and types

pub mod client;
pub mod crypto;
pub mod types;

// Integration tests (real API calls) - only compiled for test builds
#[cfg(test)]
pub mod integration_tests;

pub use client::ApiClient;
pub use types::*;

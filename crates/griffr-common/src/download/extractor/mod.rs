//! Multi-volume ZIP indexing and range-aware extraction.

mod extract;
mod inspection;
mod layout;
mod range;

#[cfg(test)]
mod tests;

pub use extract::MultiVolumeExtractor;
pub(crate) use inspection::*;
pub use inspection::{ArchiveDirectory, ArchiveInspection};
pub use layout::MultiVolumeLayout;
pub use range::ArchiveRangeRequest;
pub(crate) use range::*;

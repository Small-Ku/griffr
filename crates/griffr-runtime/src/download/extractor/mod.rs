//! Multi-volume ZIP indexing and range-aware extraction.

mod archive_index;
mod extract;
mod layout;
mod range;

#[cfg(test)]
mod tests;

pub(crate) use archive_index::*;
pub use archive_index::{ArchiveDirectory, ArchiveIndex};
pub use extract::MultiVolumeExtractor;
pub use layout::MultiVolumeLayout;
pub use range::ArchiveRangeRequest;
pub(crate) use range::*;

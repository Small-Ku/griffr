use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path;

pub const PREDOWNLOAD_STAGE_METADATA_NAME: &str = ".griffr-predownload.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StagedArchivePart {
    pub filename: String,
    pub md5: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PredownloadStageMetadata {
    pub schema_version: u32,
    pub game: String,
    pub region: String,
    pub channel: String,
    pub sub_channel: String,
    pub source_version: String,
    pub target_version: String,
    pub archives: Vec<StagedArchivePart>,
    pub created_at: String,
}

impl PredownloadStageMetadata {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Unsupported predownload metadata schema version {}",
                    self.schema_version
                ),
            });
        }
        for (label, value) in [
            ("game", self.game.as_str()),
            ("region", self.region.as_str()),
            ("channel", self.channel.as_str()),
            ("sub_channel", self.sub_channel.as_str()),
            ("source_version", self.source_version.as_str()),
            ("target_version", self.target_version.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!("Predownload metadata field {label} must not be empty"),
                });
            }
        }
        if self.archives.is_empty() {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: "Predownload metadata must contain at least one archive part".to_string(),
            });
        }
        let mut names = BTreeSet::new();
        for part in &self.archives {
            parse_safe_relative_path("predownload metadata archive filename", &part.filename)?;
            if part.size == 0 || part.md5.trim().is_empty() || !names.insert(&part.filename) {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "Invalid or duplicate predownload archive metadata for {}",
                        part.filename
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn archives_ready(&self, stage_dir: &Path) -> Result<bool> {
        self.validate()?;
        for part in &self.archives {
            let path = stage_dir.join(&part.filename);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() && metadata.len() == part.size => {}
                Ok(_) => return Ok(false),
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
                Err(err) => {
                    return Err(Error::IoAt {
                        action: "query file metadata/stat for",
                        path,
                        source: err,
                    })
                }
            }
        }
        Ok(true)
    }
}

pub fn read_predownload_stage_metadata(stage_dir: &Path) -> Result<PredownloadStageMetadata> {
    let path = stage_dir.join(PREDOWNLOAD_STAGE_METADATA_NAME);
    let metadata: PredownloadStageMetadata =
        serde_json::from_slice(&std::fs::read(&path).map_err(|source| Error::IoAt {
            action: "open file",
            path: path.clone(),
            source,
        })?)?;
    metadata.validate()?;
    Ok(metadata)
}

pub fn write_predownload_stage_metadata(
    stage_dir: &Path,
    metadata: &PredownloadStageMetadata,
) -> Result<()> {
    metadata.validate()?;
    std::fs::create_dir_all(stage_dir).map_err(|source| Error::IoAt {
        action: "create directory",
        path: stage_dir.to_path_buf(),
        source,
    })?;
    let path = stage_dir.join(PREDOWNLOAD_STAGE_METADATA_NAME);
    let temp = stage_dir.join(format!("{PREDOWNLOAD_STAGE_METADATA_NAME}.tmp"));
    let payload = serde_json::to_vec_pretty(metadata)?;
    std::fs::write(&temp, payload).map_err(|source| Error::IoAt {
        action: "open file",
        path: temp.clone(),
        source,
    })?;
    crate::runtime::task_pool::fs_ops::extract::move_path_replace(&temp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_metadata_rejects_duplicate_parts() {
        let metadata = PredownloadStageMetadata {
            schema_version: PredownloadStageMetadata::SCHEMA_VERSION,
            game: "endfield".to_string(),
            region: "sg".to_string(),
            channel: "6".to_string(),
            sub_channel: "6".to_string(),
            source_version: "1.3.3".to_string(),
            target_version: "1.4.4".to_string(),
            archives: vec![
                StagedArchivePart {
                    filename: "a.zip.001".to_string(),
                    md5: "a".to_string(),
                    size: 1,
                },
                StagedArchivePart {
                    filename: "a.zip.001".to_string(),
                    md5: "b".to_string(),
                    size: 1,
                },
            ],
            created_at: "2026-07-16T00:00:00Z".to_string(),
        };
        assert!(metadata.validate().is_err());
    }
}

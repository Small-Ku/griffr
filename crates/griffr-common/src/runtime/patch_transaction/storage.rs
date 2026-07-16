use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path;

use super::PATCH_STORAGE_METADATA_NAME;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchStorageTopology {
    pub schema_version: u32,
    pub vfs_link: PathBuf,
    pub external_vfs_root: PathBuf,
}

impl PatchStorageTopology {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(Error::Config(format!(
                "Unsupported patch storage metadata schema version {}",
                self.schema_version
            )));
        }
        parse_safe_relative_path("external VFS link", &self.vfs_link.to_string_lossy())?;
        if !self.external_vfs_root.is_absolute() {
            return Err(Error::Config(
                "External VFS root must be an absolute path".to_string(),
            ));
        }
        Ok(())
    }
}

pub fn read_patch_storage_topology(install_root: &Path) -> Result<Option<PatchStorageTopology>> {
    let path = install_root.join(PATCH_STORAGE_METADATA_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let topology: PatchStorageTopology = serde_json::from_slice(&std::fs::read(&path).map_err(
        |source| Error::OpenFileFailed {
            path: path.clone(),
            source,
        },
    )?)?;
    topology.validate()?;
    Ok(Some(topology))
}

pub(crate) fn write_patch_storage_topology(
    install_root: &Path,
    topology: &PatchStorageTopology,
) -> Result<()> {
    topology.validate()?;
    let path = install_root.join(PATCH_STORAGE_METADATA_NAME);
    let temp = install_root.join(format!("{PATCH_STORAGE_METADATA_NAME}.tmp"));
    std::fs::write(&temp, serde_json::to_vec_pretty(topology)?).map_err(|source| {
        Error::OpenFileFailed {
            path: temp.clone(),
            source,
        }
    })?;
    crate::runtime::task_pool::fs_ops::extract::move_path_replace(&temp, &path)
}

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path;

use super::PATCH_STORAGE_METADATA_NAME;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchStorageLayout {
    pub schema_version: u32,
    pub vfs_link: PathBuf,
    pub external_vfs_root: PathBuf,
}

impl PatchStorageLayout {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Unsupported patch storage metadata schema version {}",
                    self.schema_version
                ),
            });
        }
        parse_safe_relative_path("external VFS link", &self.vfs_link.to_string_lossy())?;
        if !self.external_vfs_root.is_absolute() {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: "External VFS root must be an absolute path".to_string(),
            });
        }
        Ok(())
    }
}

pub fn read_patch_storage_layout(install_root: &Path) -> Result<Option<PatchStorageLayout>> {
    let path = install_root.join(PATCH_STORAGE_METADATA_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let storage_layout: PatchStorageLayout =
        serde_json::from_slice(&std::fs::read(&path).map_err(|source| Error::IoAt {
            action: "open file",
            path: path.clone(),
            source,
        })?)?;
    storage_layout.validate()?;
    Ok(Some(storage_layout))
}

pub(crate) fn write_patch_storage_layout(
    install_root: &Path,
    storage_layout: &PatchStorageLayout,
) -> Result<()> {
    storage_layout.validate()?;
    let path = install_root.join(PATCH_STORAGE_METADATA_NAME);
    let temp = install_root.join(format!("{PATCH_STORAGE_METADATA_NAME}.tmp"));
    std::fs::write(&temp, serde_json::to_vec_pretty(storage_layout)?).map_err(|source| {
        Error::IoAt {
            action: "open file",
            path: temp.clone(),
            source,
        }
    })?;
    crate::runtime::task_pool::fs_ops::extract::move_path_replace(&temp, &path)
}

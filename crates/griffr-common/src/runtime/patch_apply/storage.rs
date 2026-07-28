use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path;

use super::ASSET_STORAGE_METADATA_NAME;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetStorageLayout {
    pub schema_version: u32,
    pub asset_link: PathBuf,
    pub external_asset_root: PathBuf,
}

impl AssetStorageLayout {
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
        parse_safe_relative_path("external asset link", &self.asset_link.to_string_lossy())?;
        if !self.external_asset_root.is_absolute() {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: "External asset root must be an absolute path".to_string(),
            });
        }
        Ok(())
    }
}

pub fn read_asset_storage_layout(install_root: &Path) -> Result<Option<AssetStorageLayout>> {
    let path = install_root.join(ASSET_STORAGE_METADATA_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let storage_layout: AssetStorageLayout =
        serde_json::from_slice(&std::fs::read(&path).map_err(|source| Error::IoAt {
            action: "open file",
            path: path.clone(),
            source,
        })?)?;
    storage_layout.validate()?;
    Ok(Some(storage_layout))
}

pub(crate) fn write_asset_storage_layout(
    install_root: &Path,
    storage_layout: &AssetStorageLayout,
) -> Result<()> {
    storage_layout.validate()?;
    let path = install_root.join(ASSET_STORAGE_METADATA_NAME);
    let payload = serde_json::to_vec_pretty(storage_layout)?;
    crate::runtime::task_pool::fs_ops::write_atomic_bytes(&path, &payload)
}

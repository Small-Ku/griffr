use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::task_pool::fs_ops::path_safety::parse_safe_relative_path;

use super::{ASSET_STORAGE_METADATA_NAME, ASSET_STORAGE_OWNER_NAME};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetStorageLayout {
    pub schema_version: u32,
    pub asset_link: PathBuf,
    pub external_asset_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AssetStorageOwner {
    schema_version: u32,
    storage_id: String,
    install_root: PathBuf,
}

impl AssetStorageLayout {
    pub const SCHEMA_VERSION: u32 = 1;
    const OWNER_SCHEMA_VERSION: u32 = 1;

    pub fn new_owned(
        install_root: &Path,
        asset_link: PathBuf,
        external_asset_root: PathBuf,
    ) -> Result<Self> {
        let canonical_install_root = canonical_existing_path(install_root)?;
        let canonical_external_root = canonical_existing_path(&external_asset_root)?;
        let storage_id = generate_storage_id(&canonical_install_root, &canonical_external_root);
        let layout = Self {
            schema_version: Self::SCHEMA_VERSION,
            asset_link,
            external_asset_root: canonical_external_root,
            storage_id: Some(storage_id),
            install_root: Some(canonical_install_root),
        };
        layout.validate()?;
        Ok(layout)
    }

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
        match (&self.storage_id, &self.install_root) {
            (Some(storage_id), Some(install_root)) => {
                if storage_id.is_empty() {
                    return Err(Error::Message {
                        context: "Configuration error: ",
                        detail: "External asset storage ID must not be empty".to_string(),
                    });
                }
                if !install_root.is_absolute() {
                    return Err(Error::Message {
                        context: "Configuration error: ",
                        detail: "External asset owner install root must be absolute".to_string(),
                    });
                }
            }
            (None, None) => {}
            _ => {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: "External asset ownership metadata is incomplete".to_string(),
                })
            }
        }
        Ok(())
    }

    /// Return true only when both the install-side metadata and the external
    /// root sentinel prove that the storage belongs to this exact install.
    /// Legacy layouts without ownership fields deliberately return false.
    pub fn owns_external_root(&self, install_root: &Path) -> Result<bool> {
        self.validate()?;
        let (Some(storage_id), Some(expected_install_root)) =
            (&self.storage_id, &self.install_root)
        else {
            return Ok(false);
        };
        let actual_install_root = canonical_existing_path(install_root)?;
        if &actual_install_root != expected_install_root {
            return Ok(false);
        }

        let owner_path = self.external_asset_root.join(ASSET_STORAGE_OWNER_NAME);
        let payload = match std::fs::read(&owner_path) {
            Ok(payload) => payload,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(Error::IoAt {
                    action: "open file",
                    path: owner_path,
                    source,
                })
            }
        };
        let owner: AssetStorageOwner = serde_json::from_slice(&payload)?;
        Ok(owner.schema_version == Self::OWNER_SCHEMA_VERSION
            && owner.storage_id == *storage_id
            && owner.install_root == actual_install_root)
    }

    pub fn remove_owner_sentinel_if_owned(&self, install_root: &Path) -> Result<bool> {
        if !self.owns_external_root(install_root)? {
            return Ok(false);
        }
        let owner_path = self.external_asset_root.join(ASSET_STORAGE_OWNER_NAME);
        match std::fs::remove_file(&owner_path) {
            Ok(()) => Ok(true),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::IoAt {
                action: "remove file",
                path: owner_path,
                source,
            }),
        }
    }

    fn owner(&self) -> Option<AssetStorageOwner> {
        Some(AssetStorageOwner {
            schema_version: Self::OWNER_SCHEMA_VERSION,
            storage_id: self.storage_id.clone()?,
            install_root: self.install_root.clone()?,
        })
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
    if let Some(owner) = storage_layout.owner() {
        std::fs::create_dir_all(&storage_layout.external_asset_root).map_err(|source| {
            Error::IoAt {
                action: "create directory",
                path: storage_layout.external_asset_root.clone(),
                source,
            }
        })?;
        let owner_path = storage_layout
            .external_asset_root
            .join(ASSET_STORAGE_OWNER_NAME);
        let owner_payload = serde_json::to_vec_pretty(&owner)?;
        crate::task_pool::fs_ops::write_atomic_bytes(&owner_path, &owner_payload)?;
    }

    let path = install_root.join(ASSET_STORAGE_METADATA_NAME);
    let payload = serde_json::to_vec_pretty(storage_layout)?;
    crate::task_pool::fs_ops::write_atomic_bytes(&path, &payload)
}

fn canonical_existing_path(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(|source| Error::IoAt {
        action: "canonicalize path",
        path: path.to_path_buf(),
        source,
    })
}

fn generate_storage_id(install_root: &Path, external_root: &Path) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = Md5::new();
    hasher.update(install_root.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    hasher.update(external_root.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(now.to_le_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_layout_is_readable_but_not_owned() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        let external = temp.path().join("external");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let layout = AssetStorageLayout {
            schema_version: AssetStorageLayout::SCHEMA_VERSION,
            asset_link: PathBuf::from("Data/StreamingAssets"),
            external_asset_root: external,
            storage_id: None,
            install_root: None,
        };

        assert!(!layout.owns_external_root(&install).unwrap());
    }

    #[test]
    fn owner_sentinel_must_match_both_storage_and_install() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        let other_install = temp.path().join("other-install");
        let external = temp.path().join("external");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&other_install).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let layout = AssetStorageLayout::new_owned(
            &install,
            PathBuf::from("Data/StreamingAssets"),
            external,
        )
        .unwrap();
        write_asset_storage_layout(&install, &layout).unwrap();

        assert!(layout.owns_external_root(&install).unwrap());
        assert!(!layout.owns_external_root(&other_install).unwrap());
    }
}

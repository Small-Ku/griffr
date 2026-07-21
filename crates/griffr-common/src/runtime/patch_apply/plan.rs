use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path;

use super::{PATCH_PLAN_NAME, PATCH_WORK_DIR};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchApplyOptions {
    pub work_dir: Option<PathBuf>,
    pub external_vfs_root: Option<PathBuf>,
}

impl PatchApplyOptions {
    fn normalize_absolute_path(path: &Path) -> Result<PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|source| Error::IoAt {
                    action: "query file metadata/stat for",
                    path: path.to_path_buf(),
                    source,
                })?
                .join(path)
        };
        let mut normalized = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir if normalized.pop() => {}
                Component::ParentDir => {
                    return Err(Error::Message {
                        context: "Configuration error: ",
                        detail: format!("Path {} escapes its filesystem root", path.display()),
                    });
                }
                Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                    normalized.push(component.as_os_str());
                }
            }
        }
        Ok(normalized)
    }

    pub fn resolved_for_install(&self, install_root: &Path) -> Result<Self> {
        let install_root = Self::normalize_absolute_path(install_root)?;
        let work_dir = self
            .work_dir
            .as_deref()
            .map(Self::normalize_absolute_path)
            .transpose()?;
        let external_vfs_root = self
            .external_vfs_root
            .as_deref()
            .map(Self::normalize_absolute_path)
            .transpose()?;

        if let Some(work_dir) = work_dir.as_deref() {
            if work_dir.starts_with(&install_root) {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "Patch work directory {} must be outside install root {}",
                        work_dir.display(),
                        install_root.display()
                    ),
                });
            }
        }
        if let Some(external_vfs_root) = external_vfs_root.as_deref() {
            if external_vfs_root.starts_with(&install_root) {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "External VFS root {} must be outside install root {}",
                        external_vfs_root.display(),
                        install_root.display()
                    ),
                });
            }
        }
        if let (Some(work_dir), Some(external_vfs_root)) =
            (work_dir.as_deref(), external_vfs_root.as_deref())
        {
            if work_dir.starts_with(external_vfs_root) || external_vfs_root.starts_with(work_dir) {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "Patch work directory {} and external VFS root {} must not overlap",
                        work_dir.display(),
                        external_vfs_root.display()
                    ),
                });
            }
        }

        Ok(Self {
            work_dir,
            external_vfs_root,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedPatchSource {
    AlreadyPresent,
    Local {
        payload: PathBuf,
    },
    Hdiff {
        base: PathBuf,
        payload: PathBuf,
        base_md5: String,
        base_size: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedPatchEntry {
    pub name: String,
    pub destination: PathBuf,
    pub expected_md5: String,
    pub expected_size: u64,
    pub source: PlannedPatchSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchPlan {
    pub schema_version: u32,
    pub install_root: PathBuf,
    pub stage_root: PathBuf,
    pub vfs_base_path: PathBuf,
    pub vfs_destination: PathBuf,
    pub work_dir: Option<PathBuf>,
    pub entries: Vec<PlannedPatchEntry>,
    pub delete_paths: Vec<PathBuf>,
    pub deferred_paths: Vec<PathBuf>,
}

impl PatchPlan {
    pub const SCHEMA_VERSION: u32 = 2;

    pub fn plan_path(&self) -> PathBuf {
        self.install_root.join(PATCH_WORK_DIR).join(PATCH_PLAN_NAME)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Unsupported patch plan schema version {}",
                    self.schema_version
                ),
            });
        }
        if !self.install_root.is_absolute()
            || !self.stage_root.is_absolute()
            || !self.vfs_destination.is_absolute()
            || self
                .work_dir
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
        {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: "Patch plan contains a non-absolute runtime path".to_string(),
            });
        }
        if self.stage_root == self.install_root || self.install_root.starts_with(&self.stage_root) {
            return Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "Patch staging directory {} must not be the install root or its ancestor",
                    self.stage_root.display()
                ),
            });
        }
        let vfs_base_path = parse_safe_relative_path(
            "patch plan vfs_base_path",
            &self.vfs_base_path.to_string_lossy(),
        )?;
        let mut names = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        for entry in &self.entries {
            let relative = parse_safe_relative_path("patch plan entry name", &entry.name)?;
            if entry.expected_md5.trim().is_empty() || entry.expected_size == 0 {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "Patch plan entry {} has invalid expected metadata",
                        entry.name
                    ),
                });
            }
            if !names.insert(entry.name.clone()) || !destinations.insert(entry.destination.clone())
            {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!("Patch plan contains a duplicate writer for {}", entry.name),
                });
            }
            let expected_destination = self.vfs_destination.join(&relative);
            if entry.destination != expected_destination {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "Patch plan destination {} does not match expected {}",
                        entry.destination.display(),
                        expected_destination.display()
                    ),
                });
            }
            match &entry.source {
                PlannedPatchSource::AlreadyPresent => {}
                PlannedPatchSource::Local { payload } => {
                    parse_safe_relative_path(
                        "patch plan local payload",
                        &payload.to_string_lossy(),
                    )?;
                }
                PlannedPatchSource::Hdiff {
                    base,
                    payload,
                    base_md5,
                    base_size,
                } => {
                    if !base.starts_with(&self.vfs_destination) {
                        return Err(Error::Message {
                            context: "Configuration error: ",
                            detail: format!(
                                "Patch plan base {} is outside VFS destination {}",
                                base.display(),
                                self.vfs_destination.display()
                            ),
                        });
                    }
                    parse_safe_relative_path(
                        "patch plan HDiff payload",
                        &payload.to_string_lossy(),
                    )?;
                    if base_md5.trim().is_empty() || *base_size == 0 {
                        return Err(Error::Message {
                            context: "Configuration error: ",
                            detail: format!(
                                "Patch plan base {} has invalid expected metadata",
                                base.display()
                            ),
                        });
                    }
                }
            }
        }

        let logical_vfs_root = self.install_root.join(vfs_base_path);
        let logical_outputs = self
            .entries
            .iter()
            .map(|entry| logical_vfs_root.join(&entry.name))
            .collect::<BTreeSet<_>>();
        let mut delete_paths = BTreeSet::new();
        for relative in &self.delete_paths {
            let parsed =
                parse_safe_relative_path("patch plan delete path", &relative.to_string_lossy())?;
            if !delete_paths.insert(parsed.clone()) {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "Patch plan contains duplicate delete path {}",
                        parsed.display()
                    ),
                });
            }
            if logical_outputs.contains(&self.install_root.join(&parsed)) {
                return Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!("Patch plan deletes planned output {}", parsed.display()),
                });
            }
        }
        for relative in &self.deferred_paths {
            parse_safe_relative_path("patch plan deferred path", &relative.to_string_lossy())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchCheckReport {
    pub archive_uncompressed_bytes: u64,
    pub estimated_final_growth_bytes: u64,
    pub estimated_install_peak_bytes: u64,
    pub estimated_vfs_peak_bytes: u64,
    pub estimated_work_bytes: u64,
    pub available_install_bytes: Option<u64>,
    pub available_vfs_bytes: Option<u64>,
    pub available_work_bytes: Option<u64>,
    pub patch_entries: usize,
    pub delete_entries: usize,
}

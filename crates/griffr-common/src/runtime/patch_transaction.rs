use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::types::{ResourcePatch, ResourcePatchEntry};
use crate::download::extractor::ArchiveInspection;
use crate::error::{Error, Result};
use crate::runtime::task_pool::verify::build_issue;
use crate::runtime::{
    DELETE_FILES_MANIFEST_NAME, PATCH_DIFF_STAGE_DIR, PATCH_FILES_STAGE_DIR, PATCH_MANIFEST_NAME,
    PATCH_STAGE_DIR,
};

use super::task_pool::fs_ops::{
    parse_delete_files_manifest, path_safety::parse_safe_relative_path,
};

pub const PREDOWNLOAD_STAGE_METADATA_NAME: &str = ".griffr-predownload.json";
pub const PATCH_TRANSACTION_DIR: &str = ".griffr-patch";
pub const PATCH_DEFERRED_DIR: &str = "deferred";
pub const PATCH_PLAN_NAME: &str = "plan.json";
pub const PATCH_STORAGE_METADATA_NAME: &str = ".griffr-storage.json";

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
            return Err(Error::Config(format!(
                "Unsupported predownload metadata schema version {}",
                self.schema_version
            )));
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
                return Err(Error::Config(format!(
                    "Predownload metadata field {label} must not be empty"
                )));
            }
        }
        if self.archives.is_empty() {
            return Err(Error::Config(
                "Predownload metadata must contain at least one archive part".to_string(),
            ));
        }
        let mut names = BTreeSet::new();
        for part in &self.archives {
            parse_safe_relative_path("predownload metadata archive filename", &part.filename)?;
            if part.size == 0 || part.md5.trim().is_empty() || !names.insert(&part.filename) {
                return Err(Error::Config(format!(
                    "Invalid or duplicate predownload archive metadata for {}",
                    part.filename
                )));
            }
        }
        Ok(())
    }

    pub fn archives_complete(&self, stage_dir: &Path) -> Result<bool> {
        self.validate()?;
        for part in &self.archives {
            let path = stage_dir.join(&part.filename);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() && metadata.len() == part.size => {}
                Ok(_) => return Ok(false),
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
                Err(err) => return Err(Error::StatFailed { path, source: err }),
            }
        }
        Ok(true)
    }
}

pub fn read_predownload_stage_metadata(stage_dir: &Path) -> Result<PredownloadStageMetadata> {
    let path = stage_dir.join(PREDOWNLOAD_STAGE_METADATA_NAME);
    let metadata: PredownloadStageMetadata = serde_json::from_slice(
        &std::fs::read(&path).map_err(|source| Error::OpenFileFailed {
            path: path.clone(),
            source,
        })?,
    )?;
    metadata.validate()?;
    Ok(metadata)
}

pub fn write_predownload_stage_metadata(
    stage_dir: &Path,
    metadata: &PredownloadStageMetadata,
) -> Result<()> {
    metadata.validate()?;
    std::fs::create_dir_all(stage_dir).map_err(|source| Error::CreateDirFailed {
        path: stage_dir.to_path_buf(),
        source,
    })?;
    let path = stage_dir.join(PREDOWNLOAD_STAGE_METADATA_NAME);
    let temp = stage_dir.join(format!("{PREDOWNLOAD_STAGE_METADATA_NAME}.tmp"));
    let payload = serde_json::to_vec_pretty(metadata)?;
    std::fs::write(&temp, payload).map_err(|source| Error::OpenFileFailed {
        path: temp.clone(),
        source,
    })?;
    super::task_pool::fs_ops::extract::move_path_replace(&temp, &path)?;
    Ok(())
}

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
                .map_err(|source| Error::StatFailed {
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
                    return Err(Error::Config(format!(
                        "Path {} escapes its filesystem root",
                        path.display()
                    )));
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
                return Err(Error::Config(format!(
                    "Patch work directory {} must be outside install root {}",
                    work_dir.display(),
                    install_root.display()
                )));
            }
        }
        if let Some(external_vfs_root) = external_vfs_root.as_deref() {
            if external_vfs_root.starts_with(&install_root) {
                return Err(Error::Config(format!(
                    "External VFS root {} must be outside install root {}",
                    external_vfs_root.display(),
                    install_root.display()
                )));
            }
        }
        if let (Some(work_dir), Some(external_vfs_root)) =
            (work_dir.as_deref(), external_vfs_root.as_deref())
        {
            if work_dir.starts_with(external_vfs_root) || external_vfs_root.starts_with(work_dir) {
                return Err(Error::Config(format!(
                    "Patch work directory {} and external VFS root {} must not overlap",
                    work_dir.display(),
                    external_vfs_root.display()
                )));
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
    AlreadyMaterialized,
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
pub struct PatchExecutionPlan {
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

impl PatchExecutionPlan {
    pub const SCHEMA_VERSION: u32 = 2;

    pub fn plan_path(&self) -> PathBuf {
        self.install_root
            .join(PATCH_TRANSACTION_DIR)
            .join(PATCH_PLAN_NAME)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(Error::Config(format!(
                "Unsupported patch plan schema version {}",
                self.schema_version
            )));
        }
        if !self.install_root.is_absolute()
            || !self.stage_root.is_absolute()
            || !self.vfs_destination.is_absolute()
            || self
                .work_dir
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
        {
            return Err(Error::Config(
                "Patch plan contains a non-absolute runtime path".to_string(),
            ));
        }
        if self.stage_root == self.install_root || self.install_root.starts_with(&self.stage_root) {
            return Err(Error::Config(format!(
                "Patch staging directory {} must not be the install root or its ancestor",
                self.stage_root.display()
            )));
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
                return Err(Error::Config(format!(
                    "Patch plan entry {} has invalid expected metadata",
                    entry.name
                )));
            }
            if !names.insert(entry.name.clone()) || !destinations.insert(entry.destination.clone())
            {
                return Err(Error::Config(format!(
                    "Patch plan contains a duplicate writer for {}",
                    entry.name
                )));
            }
            let expected_destination = self.vfs_destination.join(&relative);
            if entry.destination != expected_destination {
                return Err(Error::Config(format!(
                    "Patch plan destination {} does not match expected {}",
                    entry.destination.display(),
                    expected_destination.display()
                )));
            }
            match &entry.source {
                PlannedPatchSource::AlreadyMaterialized => {}
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
                        return Err(Error::Config(format!(
                            "Patch plan base {} is outside VFS destination {}",
                            base.display(),
                            self.vfs_destination.display()
                        )));
                    }
                    parse_safe_relative_path(
                        "patch plan HDiff payload",
                        &payload.to_string_lossy(),
                    )?;
                    if base_md5.trim().is_empty() || *base_size == 0 {
                        return Err(Error::Config(format!(
                            "Patch plan base {} has invalid expected metadata",
                            base.display()
                        )));
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
                return Err(Error::Config(format!(
                    "Patch plan contains duplicate delete path {}",
                    parsed.display()
                )));
            }
            if logical_outputs.contains(&self.install_root.join(&parsed)) {
                return Err(Error::Config(format!(
                    "Patch plan deletes materialized output {}",
                    parsed.display()
                )));
            }
        }
        for relative in &self.deferred_paths {
            parse_safe_relative_path("patch plan deferred path", &relative.to_string_lossy())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchPreflightReport {
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
    super::task_pool::fs_ops::extract::move_path_replace(&temp, &path)
}

fn normalized_archive_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn archive_payload_path(stage_subdir: &str, raw: &str) -> Result<PathBuf> {
    let relative = parse_safe_relative_path("patch archive payload", raw)?;
    let stage_subdir_path = Path::new(stage_subdir);
    if relative.starts_with(PATCH_STAGE_DIR) {
        return Ok(relative);
    }
    if relative.starts_with(stage_subdir_path) {
        return Ok(Path::new(PATCH_STAGE_DIR).join(relative));
    }
    Ok(Path::new(PATCH_STAGE_DIR)
        .join(stage_subdir_path)
        .join(relative))
}

fn is_patch_archive_control_path(path: &Path) -> bool {
    path == Path::new(PATCH_MANIFEST_NAME)
        || path == Path::new(DELETE_FILES_MANIFEST_NAME)
        || path.starts_with(PATCH_STAGE_DIR)
}

fn metadata_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn directory_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| Error::ReadDirFailed {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::ReadDirFailed {
                path: directory.clone(),
                source,
            })?;
            let entry_path = entry.path();
            let metadata =
                std::fs::symlink_metadata(&entry_path).map_err(|source| Error::StatFailed {
                    path: entry_path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry_path);
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn directory_is_empty(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(true);
    }
    let mut entries = std::fs::read_dir(path).map_err(|source| Error::ReadDirFailed {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(entries.next().is_none())
}

fn same_link_target(link: &Path, target: &Path) -> bool {
    std::fs::read_link(link)
        .ok()
        .map(|existing| {
            let resolved = if existing.is_absolute() {
                existing
            } else {
                link.parent().unwrap_or(Path::new(".")).join(existing)
            };
            resolved == target
        })
        .unwrap_or(false)
}

fn same_storage_volume(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let volume = |path: &Path| {
            path.components()
                .next()
                .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
        };
        volume(left) == volume(right)
    }
    #[cfg(not(windows))]
    {
        let _ = (left, right);
        true
    }
}

fn verify_space_requirements(
    install_root: &Path,
    install_bytes: u64,
    install_available: Option<u64>,
    external: Option<(&Path, u64, Option<u64>)>,
    work: Option<(&Path, u64, Option<u64>)>,
) -> Result<()> {
    let mut groups = vec![(
        install_root.to_path_buf(),
        install_bytes,
        install_available,
        vec!["install"],
    )];
    for (path, required, available, label) in external
        .map(|(path, required, available)| (path, required, available, "VFS"))
        .into_iter()
        .chain(work.map(|(path, required, available)| (path, required, available, "work")))
    {
        if required == 0 {
            continue;
        }
        if let Some(group) = groups
            .iter_mut()
            .find(|(group_path, ..)| same_storage_volume(group_path, path))
        {
            group.1 = group.1.saturating_add(required);
            group.3.push(label);
        } else {
            groups.push((path.to_path_buf(), required, available, vec![label]));
        }
    }

    for (path, required, available, labels) in groups {
        if available.is_some_and(|available| available < required) {
            return Err(Error::Vfs(format!(
                "Patch preflight requires approximately {} bytes of peak free space for {} data on the volume containing {}, but only {} bytes are available",
                required,
                labels.join(" + "),
                path.display(),
                available.unwrap_or_default()
            )));
        }
    }
    Ok(())
}

pub fn preflight_patch_archives(
    volumes: Vec<PathBuf>,
    install_root: &Path,
    password: Option<&str>,
    options: &PatchApplyOptions,
) -> Result<PatchPreflightReport> {
    let options = options.resolved_for_install(install_root)?;
    let extractor = crate::download::extractor::MultiVolumeExtractor::new(volumes)?;
    let inspection = extractor.inspect_patch_payload(password)?;
    let stage_root = options
        .work_dir
        .as_deref()
        .unwrap_or_else(|| install_root.parent().unwrap_or(install_root))
        .join(".griffr-preflight");
    let (_, report) = build_patch_execution_plan(install_root, &stage_root, &inspection, &options)?;
    Ok(report)
}

pub(crate) fn build_patch_execution_plan(
    install_root: &Path,
    stage_root: &Path,
    inspection: &ArchiveInspection,
    options: &PatchApplyOptions,
) -> Result<(PatchExecutionPlan, PatchPreflightReport)> {
    let mut options = options.resolved_for_install(install_root)?;
    if let Some(topology) = read_patch_storage_topology(install_root)? {
        match options.external_vfs_root.as_ref() {
            Some(requested) if requested != &topology.external_vfs_root => {
                return Err(Error::Config(format!(
                    "Install already manages VFS storage at {}; requested external root {} does not match",
                    topology.external_vfs_root.display(),
                    requested.display()
                )));
            }
            Some(_) => {}
            None => options.external_vfs_root = Some(topology.external_vfs_root),
        }
    }
    let manifest = inspection
        .patch_manifest
        .as_ref()
        .ok_or_else(|| Error::Vfs(format!("Patch archive is missing {PATCH_MANIFEST_NAME}")))?;
    let vfs_base_path =
        parse_safe_relative_path("patch.json vfs_base_path", manifest.vfs_base_path.trim())?;
    let logical_vfs_destination = install_root.join(&vfs_base_path);
    let vfs_destination = options
        .external_vfs_root
        .clone()
        .unwrap_or_else(|| logical_vfs_destination.clone());
    if options.external_vfs_root.is_some() {
        if vfs_destination == logical_vfs_destination || vfs_destination.starts_with(install_root) {
            return Err(Error::Vfs(format!(
                "External VFS root {} must be outside the install root and differ from {}",
                vfs_destination.display(),
                logical_vfs_destination.display()
            )));
        }
        if !directory_is_empty(&vfs_destination)?
            && !same_link_target(&logical_vfs_destination, &vfs_destination)
        {
            return Err(Error::Vfs(format!(
                "External VFS root {} is not empty and is not the current target of {}",
                vfs_destination.display(),
                logical_vfs_destination.display()
            )));
        }
    }
    let delete_paths =
        parse_delete_files_manifest(inspection.delete_manifest.as_deref().unwrap_or_default())?;
    let delete_set = delete_paths.iter().cloned().collect::<BTreeSet<_>>();
    let archive_entries = &inspection.entries;
    let top_level_growth = archive_entries
        .iter()
        .filter_map(|(name, size)| {
            let relative = PathBuf::from(name);
            if is_patch_archive_control_path(&relative) {
                return None;
            }
            let existing = metadata_len(&install_root.join(&relative));
            Some(size.saturating_sub(existing))
        })
        .sum::<u64>();
    let mut entries = Vec::with_capacity(manifest.files.len());
    let mut missing = Vec::new();
    let mut max_output = 0u64;
    let mut final_delta: i128 = 0;

    for entry in &manifest.files {
        let relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
        let destination = vfs_destination.join(&relative);
        let logical_destination = logical_vfs_destination.join(&relative);
        let logical = relative.to_string_lossy().replace('\\', "/");
        let existing_path = if options.external_vfs_root.is_some() {
            &logical_destination
        } else {
            &destination
        };
        let existing_size = metadata_len(existing_path);
        max_output = max_output.max(entry.size);
        final_delta += i128::from(entry.size) - i128::from(existing_size);

        let source = if build_issue(existing_path, &logical, &entry.md5, Some(entry.size)).is_none()
        {
            PlannedPatchSource::AlreadyMaterialized
        } else if let Some(local_path) = entry.effective_local_path() {
            let archive_path = archive_payload_path(PATCH_FILES_STAGE_DIR, local_path)?;
            if !archive_entries.contains_key(&normalized_archive_path(&archive_path)) {
                missing.push(format!(
                    "{} (missing local payload {})",
                    entry.name,
                    archive_path.display()
                ));
                continue;
            }
            PlannedPatchSource::Local {
                payload: archive_path,
            }
        } else {
            let mut selected = None;
            let mut failures = Vec::new();
            for diff in &entry.patch {
                let base_relative =
                    parse_safe_relative_path("patch.json base_file", diff.effective_base_file())?;
                let verified_base = logical_vfs_destination.join(&base_relative);
                let planned_base = vfs_destination.join(&base_relative);
                let base_logical = base_relative.to_string_lossy().replace('\\', "/");
                let payload =
                    archive_payload_path(PATCH_DIFF_STAGE_DIR, diff.effective_patch_path())?;
                if build_issue(
                    &verified_base,
                    &base_logical,
                    &diff.base_md5,
                    Some(diff.base_size),
                )
                .is_some()
                {
                    failures.push(format!("{} (base mismatch)", base_relative.display()));
                    continue;
                }
                if !archive_entries.contains_key(&normalized_archive_path(&payload)) {
                    failures.push(format!("{} (payload missing)", payload.display()));
                    continue;
                }
                selected = Some(PlannedPatchSource::Hdiff {
                    base: planned_base,
                    payload,
                    base_md5: diff.base_md5.clone(),
                    base_size: diff.base_size,
                });
                break;
            }
            match selected {
                Some(source) => source,
                None => {
                    missing.push(format!(
                        "{} ({})",
                        entry.name,
                        if failures.is_empty() {
                            "no patch candidates".to_string()
                        } else {
                            failures.join("; ")
                        }
                    ));
                    continue;
                }
            }
        };
        entries.push(PlannedPatchEntry {
            name: entry.name.clone(),
            destination,
            expected_md5: entry.md5.clone(),
            expected_size: entry.size,
            source,
        });
    }

    if !missing.is_empty() {
        return Err(Error::Vfs(format!(
            "Patch preflight found unrecoverable entries: {}",
            missing.join(", ")
        )));
    }

    let logical_outputs = entries
        .iter()
        .map(|entry| {
            install_root
                .join(&vfs_base_path)
                .join(Path::new(&entry.name))
        })
        .collect::<BTreeSet<_>>();
    let conflicting_deletes = delete_paths
        .iter()
        .filter(|relative| logical_outputs.contains(&install_root.join(relative)))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if !conflicting_deletes.is_empty() {
        return Err(Error::Vfs(format!(
            "Delete manifest conflicts with patch outputs: {}",
            conflicting_deletes.join(", ")
        )));
    }

    let vfs_growth = final_delta.max(0) as u64;
    let existing_vfs_bytes = if options.external_vfs_root.is_some() {
        directory_size(&logical_vfs_destination)?
    } else {
        0
    };
    let delete_bytes = delete_set
        .iter()
        .map(|relative| metadata_len(&install_root.join(relative)))
        .sum::<u64>();
    let final_growth = top_level_growth
        .saturating_add(vfs_growth)
        .saturating_sub(delete_bytes);
    let max_top_level_output = archive_entries
        .iter()
        .filter_map(|(name, size)| {
            let relative = PathBuf::from(name);
            (!is_patch_archive_control_path(&relative)).then_some(*size)
        })
        .max()
        .unwrap_or(0);
    let extraction_on_install_volume = options.work_dir.is_none();
    let install_vfs_peak = if options.external_vfs_root.is_some() {
        0
    } else {
        vfs_growth.saturating_add(max_output)
    };
    let install_peak = top_level_growth
        .saturating_add(install_vfs_peak)
        .saturating_add(if extraction_on_install_volume {
            inspection.total_uncompressed_bytes
        } else {
            max_top_level_output
        });
    let vfs_peak = if options.external_vfs_root.is_some() {
        existing_vfs_bytes
            .saturating_add(vfs_growth)
            .saturating_add(max_output)
    } else {
        install_peak
    };
    let work_bytes = if options.work_dir.is_some() {
        inspection
            .total_uncompressed_bytes
            .saturating_add(max_output)
    } else {
        0
    };
    let available_install_bytes = available_space(install_root)?;
    let available_vfs_bytes = match options.external_vfs_root.as_deref() {
        Some(path) => available_space(path)?,
        None => available_install_bytes,
    };
    let available_work_bytes = match options.work_dir.as_deref() {
        Some(path) => available_space(path)?,
        None => available_install_bytes,
    };
    verify_space_requirements(
        install_root,
        install_peak,
        available_install_bytes,
        options
            .external_vfs_root
            .as_deref()
            .map(|path| (path, vfs_peak, available_vfs_bytes)),
        options
            .work_dir
            .as_deref()
            .map(|path| (path, work_bytes, available_work_bytes)),
    )?;

    let deferred_paths = [PathBuf::from("config.ini")]
        .into_iter()
        .filter(|path| archive_entries.contains_key(&normalized_archive_path(path)))
        .collect::<Vec<_>>();
    let plan = PatchExecutionPlan {
        schema_version: PatchExecutionPlan::SCHEMA_VERSION,
        install_root: install_root.to_path_buf(),
        stage_root: stage_root.to_path_buf(),
        vfs_base_path,
        vfs_destination,
        work_dir: options.work_dir.clone(),
        entries,
        delete_paths,
        deferred_paths,
    };
    plan.validate()?;
    let report = PatchPreflightReport {
        archive_uncompressed_bytes: inspection.total_uncompressed_bytes,
        estimated_final_growth_bytes: final_growth,
        estimated_install_peak_bytes: install_peak,
        estimated_vfs_peak_bytes: vfs_peak,
        estimated_work_bytes: work_bytes,
        available_install_bytes,
        available_vfs_bytes,
        available_work_bytes,
        patch_entries: plan.entries.len(),
        delete_entries: plan.delete_paths.len(),
    };
    Ok((plan, report))
}

pub(crate) fn write_patch_execution_plan(plan: &PatchExecutionPlan) -> Result<()> {
    plan.validate()?;
    let transaction_dir = plan.install_root.join(PATCH_TRANSACTION_DIR);
    std::fs::create_dir_all(&transaction_dir).map_err(|source| Error::CreateDirFailed {
        path: transaction_dir.clone(),
        source,
    })?;
    let path = transaction_dir.join(PATCH_PLAN_NAME);
    let temp = transaction_dir.join(format!("{PATCH_PLAN_NAME}.tmp"));
    std::fs::write(&temp, serde_json::to_vec_pretty(plan)?).map_err(|source| {
        Error::OpenFileFailed {
            path: temp.clone(),
            source,
        }
    })?;
    super::task_pool::fs_ops::extract::move_path_replace(&temp, &path)
}

pub(crate) fn read_patch_execution_plan(install_root: &Path) -> Result<PatchExecutionPlan> {
    let path = install_root
        .join(PATCH_TRANSACTION_DIR)
        .join(PATCH_PLAN_NAME);
    let plan: PatchExecutionPlan = serde_json::from_slice(&std::fs::read(&path).map_err(
        |source| Error::OpenFileFailed {
            path: path.clone(),
            source,
        },
    )?)?;
    plan.validate()?;
    Ok(plan)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchRecoveryState {
    ArchiveReady { stage_dir: PathBuf },
    ExtractedReady,
    ExtractedIncomplete { missing: Vec<String> },
    DeletePending,
    Complete,
    Inconsistent { reasons: Vec<String> },
}

fn resolve_stage_payload(
    install_root: &Path,
    stage_root: &Path,
    stage_subdir: &str,
    label: &str,
    raw: &str,
) -> Result<PathBuf> {
    let relative = parse_safe_relative_path(label, raw)?;
    let subdir = Path::new(stage_subdir);
    if relative.starts_with(PATCH_STAGE_DIR) {
        return Ok(install_root.join(relative));
    }
    if relative.starts_with(subdir) {
        return Ok(stage_root.join(relative));
    }
    Ok(stage_root.join(stage_subdir).join(relative))
}

fn entry_is_recoverable(
    install_root: &Path,
    stage_root: &Path,
    dest_root: &Path,
    entry: &ResourcePatchEntry,
) -> Result<bool> {
    let relative = parse_safe_relative_path("patch.json file name", &entry.name)?;
    let destination = dest_root.join(&relative);
    let logical = relative.to_string_lossy().replace('\\', "/");
    if build_issue(&destination, &logical, &entry.md5, Some(entry.size)).is_none() {
        return Ok(true);
    }
    if let Some(local_path) = entry.effective_local_path() {
        return Ok(resolve_stage_payload(
            install_root,
            stage_root,
            PATCH_FILES_STAGE_DIR,
            "patch.json local_path",
            local_path,
        )?
        .is_file());
    }
    for diff in &entry.patch {
        let base_relative =
            parse_safe_relative_path("patch.json base_file", diff.effective_base_file())?;
        let base_path = dest_root.join(&base_relative);
        let base_logical = base_relative.to_string_lossy().replace('\\', "/");
        let patch_path = resolve_stage_payload(
            install_root,
            stage_root,
            PATCH_DIFF_STAGE_DIR,
            "patch.json patch path",
            diff.effective_patch_path(),
        )?;
        if build_issue(
            &base_path,
            &base_logical,
            &diff.base_md5,
            Some(diff.base_size),
        )
        .is_none()
            && patch_path.is_file()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn classify_patch_recovery(
    install_root: &Path,
    stage_dir: Option<&Path>,
) -> Result<PatchRecoveryState> {
    let manifest_path = install_root.join(PATCH_MANIFEST_NAME);
    let stage_root = install_root.join(PATCH_STAGE_DIR);
    let delete_manifest = install_root.join(DELETE_FILES_MANIFEST_NAME);
    let deferred = install_root
        .join(PATCH_TRANSACTION_DIR)
        .join(PATCH_DEFERRED_DIR);

    let plan_path = install_root
        .join(PATCH_TRANSACTION_DIR)
        .join(PATCH_PLAN_NAME);
    if plan_path.is_file() {
        let plan = read_patch_execution_plan(install_root)?;
        let missing_stage = plan.entries.iter().any(|entry| match &entry.source {
            PlannedPatchSource::AlreadyMaterialized => false,
            PlannedPatchSource::Local { payload } => {
                !plan.stage_root.join(payload).is_file()
                    && build_issue(
                        &entry.destination,
                        &entry.name,
                        &entry.expected_md5,
                        Some(entry.expected_size),
                    )
                    .is_some()
            }
            PlannedPatchSource::Hdiff {
                base,
                payload,
                base_md5,
                base_size,
            } => {
                let output_incomplete = build_issue(
                    &entry.destination,
                    &entry.name,
                    &entry.expected_md5,
                    Some(entry.expected_size),
                )
                .is_some();
                output_incomplete
                    && (!plan.stage_root.join(payload).is_file()
                        || build_issue(base, &entry.name, base_md5, Some(*base_size)).is_some())
            }
        });
        return if missing_stage {
            Ok(PatchRecoveryState::ExtractedIncomplete {
                missing: vec![format!(
                    "Patch transaction staging is incomplete at {}",
                    plan.stage_root.display()
                )],
            })
        } else {
            Ok(PatchRecoveryState::ExtractedReady)
        };
    }

    if manifest_path.is_file() {
        let manifest: ResourcePatch =
            serde_json::from_slice(&std::fs::read(&manifest_path).map_err(|source| {
                Error::OpenFileFailed {
                    path: manifest_path.clone(),
                    source,
                }
            })?)?;
        let base = parse_safe_relative_path("patch.json vfs_base_path", &manifest.vfs_base_path)?;
        let dest_root = install_root.join(base);
        let mut missing = Vec::new();
        for entry in &manifest.files {
            if !entry_is_recoverable(install_root, &stage_root, &dest_root, entry)? {
                missing.push(entry.name.clone());
            }
        }
        if missing.is_empty() {
            return Ok(PatchRecoveryState::ExtractedReady);
        }
        return Ok(PatchRecoveryState::ExtractedIncomplete { missing });
    }

    if stage_root.exists() {
        return Ok(PatchRecoveryState::Inconsistent {
            reasons: vec![format!(
                "{} exists without {}",
                stage_root.display(),
                manifest_path.display()
            )],
        });
    }
    if delete_manifest.is_file() {
        return Ok(PatchRecoveryState::DeletePending);
    }
    if deferred.exists() {
        return Ok(PatchRecoveryState::Inconsistent {
            reasons: vec![format!(
                "Deferred patch files exist at {} without a transaction plan",
                deferred.display()
            )],
        });
    }
    if let Some(stage_dir) = stage_dir {
        let metadata_path = stage_dir.join(PREDOWNLOAD_STAGE_METADATA_NAME);
        if metadata_path.is_file() {
            let metadata = read_predownload_stage_metadata(stage_dir)?;
            if metadata.archives_complete(stage_dir)? {
                return Ok(PatchRecoveryState::ArchiveReady {
                    stage_dir: stage_dir.to_path_buf(),
                });
            }
            return Ok(PatchRecoveryState::Inconsistent {
                reasons: vec![format!(
                    "Predownload archives under {} are incomplete",
                    stage_dir.display()
                )],
            });
        }
    }
    Ok(PatchRecoveryState::Complete)
}

#[cfg(windows)]
pub fn available_space(path: &Path) -> Result<Option<u64>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut probe = path;
    while !probe.exists() {
        let Some(parent) = probe.parent() else {
            break;
        };
        if parent == probe {
            break;
        }
        probe = parent;
    }
    let mut wide: Vec<u16> = probe.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut available = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Error::StatFailed {
            path: probe.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(Some(available))
}

#[cfg(not(windows))]
pub fn available_space(_path: &Path) -> Result<Option<u64>> {
    Ok(None)
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

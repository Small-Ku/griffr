use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::{
    griffr_predownload_path, read_predownload_stage_metadata, PREDOWNLOAD_STAGE_METADATA_NAME,
};
use griffr_hypergryph_api::{GameFileEntry, GetLatestGameResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivePackageKind {
    Patch,
    Full,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ManifestDeltaEstimate {
    pub changed_files: usize,
    pub direct_bytes: u64,
}

pub fn estimate_manifest_delta(
    current: &[GameFileEntry],
    target: &[GameFileEntry],
) -> ManifestDeltaEstimate {
    let mut current_by_path = rapidhash::RapidHashMap::<String, (&str, u64)>::default();
    current_by_path.reserve(current.len());
    for entry in current {
        current_by_path.insert(
            crate::normalize_logical_path(&entry.path),
            (entry.md5.as_str(), entry.size),
        );
    }

    target
        .iter()
        .fold(ManifestDeltaEstimate::default(), |mut estimate, entry| {
            let key = crate::normalize_logical_path(&entry.path);
            let unchanged = current_by_path.get(&key).is_some_and(|(md5, size)| {
                *size == entry.size && md5.eq_ignore_ascii_case(&entry.md5)
            });
            if !unchanged {
                estimate.changed_files = estimate.changed_files.saturating_add(1);
                estimate.direct_bytes = estimate.direct_bytes.saturating_add(entry.size);
            }
            estimate
        })
}

/// Treats the official patch as one group-level delivery candidate. It wins
/// only when no zero-network reuse provider is present and its declared
/// transfer is smaller than both the raw changed-file transfer and a full
/// package transfer. Per-file archive-range selection can still make the
/// manifest route cheaper, so callers should regard this as a network-first
/// heuristic rather than a content identity decision.
pub fn patch_group_beats_manifest_sources(
    version_info: &GetLatestGameResponse,
    current_version: Option<&str>,
    delta: ManifestDeltaEstimate,
    has_reuse_sources: bool,
) -> bool {
    if has_reuse_sources || delta.changed_files == 0 {
        return false;
    }
    let compatible = current_version
        .is_some_and(|current| !current.is_empty() && current == version_info.request_version);
    if !compatible {
        return false;
    }
    let Some(patch) = version_info.patch.as_ref() else {
        return false;
    };
    let patch_bytes = patch.patches.iter().map(|part| part.size()).sum::<u64>();
    if patch_bytes == 0 || patch_bytes >= delta.direct_bytes {
        return false;
    }
    let full_bytes = version_info
        .pkg
        .as_ref()
        .map(|pkg| pkg.packs.iter().map(|part| part.size()).sum::<u64>())
        .filter(|bytes| *bytes > 0);
    full_bytes.is_none_or(|bytes| patch_bytes < bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveDownloadSummary {
    pub label: &'static str,
    pub part_count: usize,
    pub total_size: u64,
}

pub fn staged_patch_request_version(stage_dir: &Path, target_version: &str) -> Result<String> {
    let metadata = read_predownload_stage_metadata(stage_dir).map_err(|error| Error::Message {
        context: "Configuration error: ",
        detail: format!(
            "Predownload stage {} is missing valid {} metadata: {error}",
            stage_dir.display(),
            PREDOWNLOAD_STAGE_METADATA_NAME
        ),
    })?;
    if metadata.target_version != target_version {
        return Err(Error::Message {
            context: "Configuration error: ",
            detail: format!(
                "Predownload stage {} targets {}, not installed target {}",
                stage_dir.display(),
                metadata.target_version,
                target_version
            ),
        });
    }
    if !metadata.archives_ready(stage_dir)? {
        return Err(Error::Message {
            context: "Configuration error: ",
            detail: format!(
                "Predownload stage {} does not contain every archive recorded in metadata",
                stage_dir.display()
            ),
        });
    }
    Ok(metadata.source_version)
}

pub fn resolve_staged_patch_recovery_dir(
    install_path: &Path,
    explicit_stage_dir: Option<&Path>,
    target_version: &str,
) -> Result<(PathBuf, String)> {
    if let Some(stage_dir) = explicit_stage_dir {
        let request_version = staged_patch_request_version(stage_dir, target_version)?;
        return Ok((stage_dir.to_path_buf(), request_version));
    }

    let root = griffr_predownload_path(install_path);
    let entries = std::fs::read_dir(&root).map_err(|source| Error::IoAt {
        action: "read directory",
        path: root.clone(),
        source,
    })?;
    let mut candidates = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            path.is_dir()
                .then(|| {
                    staged_patch_request_version(&path, target_version)
                        .ok()
                        .map(|version| (path, version))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));

    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(Error::Message { context: "Configuration error: ", detail: format!(
            "No ready staged predownload metadata targeting installed version {} was found under {}. Pass --stage-dir explicitly.",
            target_version,
            root.display()
        ) }),
        _ => Err(Error::Message { context: "Configuration error: ", detail: format!(
            "Multiple ready staged predownload directories target installed version {} under {}: {}. Pass --stage-dir explicitly.",
            target_version,
            root.display(),
            candidates
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ) }),
    }
}

pub fn select_archive_package(
    version_info: &GetLatestGameResponse,
    current_version: Option<&str>,
) -> Result<ArchivePackageKind> {
    let patch_matches_installed_version = current_version
        .is_some_and(|current| !current.is_empty() && current == version_info.request_version);

    if version_info.has_patch_package() && patch_matches_installed_version {
        return Ok(ArchivePackageKind::Patch);
    }
    if version_info.has_full_package() {
        return Ok(ArchivePackageKind::Full);
    }
    if version_info.has_patch_package() {
        return Err(Error::Message {
            context: "Configuration error: ",
            detail: format!(
            "Patch package was returned for request version '{}' but the installed version is '{}'",
            version_info.request_version,
            current_version.unwrap_or("<unknown>")
        ),
        });
    }

    Err(Error::Message {
        context: "Configuration error: ",
        detail: "Update is available but the API returned neither patch nor full package archives"
            .to_string(),
    })
}

pub fn selected_archive_download(
    version_info: &GetLatestGameResponse,
    package_kind: ArchivePackageKind,
) -> Option<ArchiveDownloadSummary> {
    match package_kind {
        ArchivePackageKind::Patch => {
            version_info
                .patch
                .as_ref()
                .map(|patch| ArchiveDownloadSummary {
                    label: "patch",
                    part_count: patch.patches.len(),
                    total_size: patch.patches.iter().map(|part| part.size()).sum(),
                })
        }
        ArchivePackageKind::Full => version_info.pkg.as_ref().map(|pkg| ArchiveDownloadSummary {
            label: "full",
            part_count: pkg.packs.len(),
            total_size: pkg.packs.iter().map(|part| part.size()).sum(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{write_predownload_stage_metadata, PredownloadStageMetadata, StagedArchivePart};
    use griffr_hypergryph_api::{PackFile, PackageInfo, PatchInfo};
    use tempfile::tempdir;

    fn full_package() -> PackageInfo {
        PackageInfo {
            packs: vec![PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "5".to_string(),
            }],
            total_size: "5".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: Some("def".to_string()),
        }
    }

    fn patch_package() -> PatchInfo {
        PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![
                PackFile {
                    url: "https://example.com/patch.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "3".to_string(),
                },
                PackFile {
                    url: "https://example.com/patch.zip.002".to_string(),
                    md5: "def".to_string(),
                    package_size: "4".to_string(),
                },
            ],
            total_size: "7".to_string(),
            package_size: "7".to_string(),
        }
    }

    fn game_file(path: &str, md5: &str, size: u64) -> GameFileEntry {
        GameFileEntry {
            path: path.to_string(),
            md5: md5.to_string(),
            size,
        }
    }

    fn response(full: bool, patch: bool) -> GetLatestGameResponse {
        GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: full.then(full_package),
            patch: patch.then(patch_package),
            pre_patch: None,
            state: 0,
            launcher_action: 0,
        }
    }

    fn write_stage_metadata(stage_dir: &Path, source: &str, target: &str) {
        std::fs::create_dir_all(stage_dir).unwrap();
        std::fs::write(stage_dir.join("bundle.zip.001"), b"x").unwrap();
        write_predownload_stage_metadata(
            stage_dir,
            &PredownloadStageMetadata {
                schema_version: PredownloadStageMetadata::SCHEMA_VERSION,
                game: "endfield".to_string(),
                region: "sg".to_string(),
                channel: "6".to_string(),
                sub_channel: "6".to_string(),
                source_version: source.to_string(),
                target_version: target.to_string(),
                archives: vec![StagedArchivePart {
                    filename: "bundle.zip.001".to_string(),
                    md5: "9dd4e461268c8034f5c8564e155c67a6".to_string(),
                    size: 1,
                }],
                created_at: "2026-07-16T00:00:00Z".to_string(),
            },
        )
        .unwrap();
    }

    #[test]
    fn reads_staged_transition_metadata() {
        let temp = tempdir().unwrap();
        let stage_dir = temp.path().join("opaque-stage-name");
        write_stage_metadata(&stage_dir, "1.3.3", "1.4.4");
        assert_eq!(
            staged_patch_request_version(&stage_dir, "1.4.4").unwrap(),
            "1.3.3"
        );
    }

    #[test]
    fn rejects_staged_transition_for_different_target() {
        let temp = tempdir().unwrap();
        let stage_dir = temp.path().join("opaque-stage-name");
        write_stage_metadata(&stage_dir, "1.3.3", "1.4.4");
        let error = staged_patch_request_version(&stage_dir, "1.5.0").unwrap_err();
        assert!(error
            .to_string()
            .contains("targets 1.4.4, not installed target 1.5.0"));
    }

    #[test]
    fn discovers_unique_staged_transition() {
        let temp = tempdir().unwrap();
        let root = griffr_predownload_path(temp.path());
        write_stage_metadata(&root.join("first"), "1.3.3", "1.4.4");
        write_stage_metadata(&root.join("second"), "1.4.4", "1.5.0");

        let (path, request_version) =
            resolve_staged_patch_recovery_dir(temp.path(), None, "1.4.4").unwrap();
        assert_eq!(path, root.join("first"));
        assert_eq!(request_version, "1.3.3");
    }

    #[test]
    fn selects_matching_patch_package() {
        assert_eq!(
            select_archive_package(&response(true, true), Some("1.0.13")).unwrap(),
            ArchivePackageKind::Patch
        );
    }

    #[test]
    fn falls_back_to_full_package() {
        assert_eq!(
            select_archive_package(&response(true, false), Some("1.0.13")).unwrap(),
            ArchivePackageKind::Full
        );
        assert_eq!(
            select_archive_package(&response(true, true), Some("1.0.14")).unwrap(),
            ArchivePackageKind::Full
        );
    }

    #[test]
    fn rejects_mismatched_patch_only_response() {
        let error = select_archive_package(&response(false, true), Some("1.0.14")).unwrap_err();
        assert!(error.to_string().contains("Patch package was returned"));
    }

    #[test]
    fn summarizes_selected_patch_archives() {
        let plan =
            selected_archive_download(&response(false, true), ArchivePackageKind::Patch).unwrap();
        assert_eq!(plan.label, "patch");
        assert_eq!(plan.part_count, 2);
        assert_eq!(plan.total_size, 7);
    }
    #[test]
    fn estimates_only_changed_manifest_payload() {
        let current = vec![game_file("a.bin", "aa", 100), game_file("b.bin", "bb", 200)];
        let target = vec![
            game_file("a.bin", "aa", 100),
            game_file("b.bin", "cc", 220),
            game_file("c.bin", "dd", 30),
        ];
        assert_eq!(
            estimate_manifest_delta(&current, &target),
            ManifestDeltaEstimate {
                changed_files: 2,
                direct_bytes: 250,
            }
        );
    }

    #[test]
    fn patch_is_only_a_smaller_group_candidate_without_reuse() {
        let mut release = response(true, true);
        release.pkg.as_mut().unwrap().packs[0].package_size = "500".to_string();
        release.patch.as_mut().unwrap().patches[0].package_size = "30".to_string();
        release.patch.as_mut().unwrap().patches[1].package_size = "20".to_string();
        let delta = ManifestDeltaEstimate {
            changed_files: 3,
            direct_bytes: 100,
        };
        assert!(patch_group_beats_manifest_sources(
            &release,
            Some("1.0.13"),
            delta,
            false
        ));
        assert!(!patch_group_beats_manifest_sources(
            &release,
            Some("1.0.13"),
            delta,
            true
        ));
        assert!(!patch_group_beats_manifest_sources(
            &release,
            Some("wrong"),
            delta,
            false
        ));
    }
}

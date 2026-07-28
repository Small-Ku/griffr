use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::api::client::ApiClient;
use crate::api::types::{GameFileEntry, GetLatestGameResponse, PackageInfo};
use crate::config::ApiTarget;
use crate::error::{Error, Result};
use crate::runtime::{
    is_griffr_private_path, normalize_logical_path, ArtifactClaim, ArtifactExpectation,
};

use super::artifact::physical_path_key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    pub version: String,
    pub file_path: String,
    pub game_files_md5: Option<String>,
}

fn normalize_delivery_path(value: &str) -> String {
    let trimmed = value.trim();
    let end = trimmed.find(['?', '#']).unwrap_or(trimmed.len());
    trimmed[..end].trim_end_matches('/').to_string()
}

impl ReleaseIdentity {
    fn from_package(version: &str, package: &PackageInfo) -> Self {
        Self {
            version: version.to_string(),
            file_path: normalize_delivery_path(&package.file_path),
            game_files_md5: package
                .game_files_md5
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GameManifestSnapshot {
    pub release: ReleaseIdentity,
    pub package: PackageInfo,
    pub entries: Vec<GameFileEntry>,
}

fn normalize_manifest_entries(entries: &mut [GameFileEntry]) -> Result<()> {
    let mut normalized_paths = Vec::with_capacity(entries.len());
    for entry in entries.iter_mut() {
        let relative = crate::runtime::task_pool::fs_ops::path_safety::parse_safe_relative_path(
            "target game_files entry",
            &entry.path,
        )?;
        if is_griffr_private_path(&relative) {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!(
                    "Target manifest cannot own private Griffr path {}",
                    entry.path
                ),
            });
        }
        if entry.md5.len() != 32 || !entry.md5.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!(
                    "Target manifest entry {} contains invalid MD5 {:?}",
                    entry.path, entry.md5
                ),
            });
        }
        entry.path = relative.to_string_lossy().replace('\\', "/");
        entry.md5.make_ascii_lowercase();
        normalized_paths.push(normalize_logical_path(&entry.path));
    }
    normalized_paths.sort();
    for pair in normalized_paths.windows(2) {
        if pair[0] == pair[1] {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!("Target manifest contains duplicate path {}", pair[0]),
            });
        }
        if pair[1]
            .strip_prefix(&pair[0])
            .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!(
                    "Target manifest contains conflicting file/directory paths {} and {}",
                    pair[0], pair[1]
                ),
            });
        }
    }
    Ok(())
}

impl GameManifestSnapshot {
    pub async fn fetch(api_client: &ApiClient, release: &GetLatestGameResponse) -> Result<Self> {
        let package = release.pkg.clone().ok_or_else(|| Error::Message {
            context: "API client wrapper error: ",
            detail: "No package information available".to_string(),
        })?;
        let mut entries = api_client
            .fetch_game_files(&package.file_path, package.game_files_md5.as_deref())
            .await?;
        normalize_manifest_entries(&mut entries)?;
        Ok(Self {
            release: ReleaseIdentity::from_package(&release.version, &package),
            package,
            entries,
        })
    }

    /// Refresh signed delivery URLs without changing the saved manifest.
    /// The live API response must still identify the same release and
    /// encrypted game_files payload.
    pub async fn refresh_delivery(
        &mut self,
        api_client: &ApiClient,
        target: &ApiTarget,
    ) -> Result<()> {
        let live = api_client
            .get_latest_game(target, Some(&self.release.version))
            .await?;
        let package = live.pkg.ok_or_else(|| Error::Message {
            context: "API client wrapper error: ",
            detail: format!(
                "No package information is available while refreshing delivery URLs for {}",
                self.release.version
            ),
        })?;
        let live_identity = ReleaseIdentity::from_package(&live.version, &package);
        if live_identity != self.release {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!(
                    "Live delivery metadata changed while release {} was in use",
                    self.release.version
                ),
            });
        }
        self.package = package;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentDomain {
    CoreGame,
    ResourceBaseline,
    LauncherMetadata,
    PersistentWorkingSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentProvider {
    GameManifest,
    ResourceIndex,
    Archive,
    Patch,
    PersistentPreference,
}

#[derive(Debug, Clone)]
pub struct PlannedContent {
    pub physical_path: PathBuf,
    pub expectation: ArtifactExpectation,
    pub domain: ContentDomain,
    pub provider: ContentProvider,
}

#[derive(Debug, Clone)]
pub struct ContentPlan {
    install_root: PathBuf,
    snapshot: GameManifestSnapshot,
    files: BTreeMap<String, PlannedContent>,
    resource_claims: Vec<ArtifactClaim>,
}

impl ContentPlan {
    pub fn from_snapshot(
        install_root: &Path,
        mut snapshot: GameManifestSnapshot,
        resource_claims: &[ArtifactClaim],
    ) -> Result<Self> {
        normalize_manifest_entries(&mut snapshot.entries)?;
        let mut files = BTreeMap::new();
        for entry in &snapshot.entries {
            let logical_path = normalize_logical_path(&entry.path);
            let physical_path = install_root.join(&entry.path);
            let key = physical_path_key(&physical_path);
            let domain = if crate::runtime::is_launcher_metadata_path(&entry.path) {
                ContentDomain::LauncherMetadata
            } else {
                ContentDomain::CoreGame
            };
            let planned = PlannedContent {
                physical_path,
                expectation: ArtifactExpectation::new(&logical_path, &entry.md5, Some(entry.size)),
                domain,
                provider: ContentProvider::GameManifest,
            };
            if files.insert(key, planned).is_some() {
                return Err(Error::Message {
                    context: "Integrity error: ",
                    detail: format!("Target manifest contains duplicate path {}", entry.path),
                });
            }
        }

        for claim in resource_claims {
            let key = physical_path_key(claim.path());
            if let Some(previous) = files.get(&key) {
                if previous.expectation.expected_md5() != claim.expectation().expected_md5()
                    || previous.expectation.expected_size() != claim.expectation().expected_size()
                {
                    return Err(Error::Message {
                        context: "Integrity error: ",
                        detail: format!(
                            "resource and game manifests claim {} with different expected content",
                            claim.path().display()
                        ),
                    });
                }
            }
            files.insert(
                key,
                PlannedContent {
                    physical_path: claim.path().to_path_buf(),
                    expectation: claim.expectation().clone(),
                    domain: ContentDomain::ResourceBaseline,
                    provider: ContentProvider::ResourceIndex,
                },
            );
        }

        Ok(Self {
            install_root: install_root.to_path_buf(),
            snapshot,
            files,
            resource_claims: resource_claims.to_vec(),
        })
    }

    pub fn snapshot(&self) -> &GameManifestSnapshot {
        &self.snapshot
    }

    pub async fn refresh_delivery(
        &mut self,
        api_client: &ApiClient,
        target: &ApiTarget,
    ) -> Result<()> {
        self.snapshot.refresh_delivery(api_client, target).await
    }

    pub fn install_root(&self) -> &Path {
        &self.install_root
    }

    pub fn resource_claims(&self) -> &[ArtifactClaim] {
        &self.resource_claims
    }

    /// Return the game-manifest entries whose authoritative owner remains the
    /// core game provider. Resource-index and launcher-metadata paths are
    /// closed by their own providers and must not be ensured a second time.
    pub fn core_game_entries(&self) -> Vec<GameFileEntry> {
        self.snapshot
            .entries
            .iter()
            .filter(|entry| {
                let key = physical_path_key(&self.install_root.join(&entry.path));
                self.files.get(&key).is_some_and(|planned| {
                    planned.domain == ContentDomain::CoreGame
                        && planned.provider == ContentProvider::GameManifest
                })
            })
            .cloned()
            .collect()
    }

    pub fn planned_files(&self) -> impl Iterator<Item = &PlannedContent> {
        self.files.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{GameFileEntry, PackageInfo};

    fn snapshot(entry: GameFileEntry) -> GameManifestSnapshot {
        GameManifestSnapshot {
            release: ReleaseIdentity {
                version: "1.0.0".to_string(),
                file_path: "https://example.invalid/files".to_string(),
                game_files_md5: None,
            },
            package: PackageInfo {
                packs: Vec::new(),
                total_size: "0".to_string(),
                file_path: "https://example.invalid/files".to_string(),
                game_files_md5: None,
            },
            entries: vec![entry],
        }
    }

    #[test]
    fn release_identity_ignores_signed_url_parts() {
        let package = PackageInfo {
            packs: Vec::new(),
            total_size: "0".to_string(),
            file_path: " https://example.invalid/files/?auth_key=short-lived#part ".to_string(),
            game_files_md5: Some("AABBCCDDEEFF00112233445566778899".to_string()),
        };

        let identity = ReleaseIdentity::from_package("1.0.0", &package);

        assert_eq!(identity.file_path, "https://example.invalid/files");
        assert_eq!(
            identity.game_files_md5.as_deref(),
            Some("aabbccddeeff00112233445566778899")
        );
    }

    #[test]
    fn unsafe_manifest_path_is_rejected_before_planning() {
        let root = Path::new("game");
        let entry = GameFileEntry {
            path: "../outside.bin".to_string(),
            md5: "00".repeat(16),
            size: 4,
        };

        let error = ContentPlan::from_snapshot(root, snapshot(entry), &[]).unwrap_err();

        assert!(error.to_string().contains("path"));
    }

    #[test]
    fn resource_claim_replaces_matching_game_manifest_owner() {
        let root = Path::new("game");
        let entry = GameFileEntry {
            path: "Data/VFS/a.bin".to_string(),
            md5: "00".repeat(16),
            size: 4,
        };
        let claim = ArtifactClaim::new(
            root.join(&entry.path),
            ArtifactExpectation::new(&entry.path, &entry.md5, Some(entry.size)),
        );
        let plan = ContentPlan::from_snapshot(root, snapshot(entry), &[claim]).unwrap();
        assert!(plan
            .planned_files()
            .any(|file| file.domain == ContentDomain::ResourceBaseline));
    }

    #[test]
    fn core_entries_exclude_resource_and_launcher_metadata_owners() {
        let root = Path::new("game");
        let mut target = snapshot(GameFileEntry {
            path: "core.bin".to_string(),
            md5: "00".repeat(16),
            size: 4,
        });
        target.entries.push(GameFileEntry {
            path: "config.ini".to_string(),
            md5: "11".repeat(16),
            size: 5,
        });
        target.entries.push(GameFileEntry {
            path: "Data/StreamingAssets/VFS/a.bin".to_string(),
            md5: "22".repeat(16),
            size: 6,
        });
        let claim = ArtifactClaim::new(
            root.join("Data/StreamingAssets/VFS/a.bin"),
            ArtifactExpectation::new("VFS/a.bin", "22".repeat(16), Some(6)),
        );

        let plan = ContentPlan::from_snapshot(root, target, &[claim]).unwrap();
        let entries = plan.core_game_entries();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "core.bin");
    }

    #[test]
    fn resource_claim_conflict_is_rejected_before_execution() {
        let root = Path::new("game");
        let entry = GameFileEntry {
            path: "Data/VFS/a.bin".to_string(),
            md5: "00".repeat(16),
            size: 4,
        };
        let claim = ArtifactClaim::new(
            root.join(&entry.path),
            ArtifactExpectation::new(&entry.path, "11".repeat(16), Some(entry.size)),
        );
        let error = ContentPlan::from_snapshot(root, snapshot(entry), &[claim]).unwrap_err();
        assert!(error.to_string().contains("different expected content"));
    }
}

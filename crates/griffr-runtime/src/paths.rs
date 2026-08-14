use std::path::Path;

use griffr_hypergryph_api::{CONFIG_INI_NAME, GAME_FILES_NAME, PACKAGE_FILES_NAME};

pub const GRIFFR_DIR: &str = ".griffr";
pub const GRIFFR_ARCHIVES_DIR: &str = "archives";
pub const GRIFFR_PATCH_DIR: &str = "patch";
pub const GRIFFR_PREDOWNLOAD_DIR: &str = "predownload";

pub const PATCH_MANIFEST_NAME: &str = "patch.json";
pub const PATCH_STAGE_DIR: &str = "vfs_files";
pub const PATCH_FILES_STAGE_DIR: &str = "files";
pub const PATCH_DIFF_STAGE_DIR: &str = "vfs_patch";
pub const DELETE_FILES_MANIFEST_NAME: &str = "delete_files.txt";

pub const STREAMING_ASSETS_DIR: &str = "StreamingAssets";
pub const PERSISTENT_DIR: &str = "Persistent";
pub const VFS_DIR: &str = "VFS";
pub const RESOURCE_GROUP_BASE: &str = "initial";
pub const RESOURCE_GROUP_MAIN: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceManifestKind {
    Index,
    Pref,
}

impl ResourceManifestKind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Pref => "pref",
        }
    }
}

pub fn griffr_path(install_root: &Path) -> std::path::PathBuf {
    install_root.join(GRIFFR_DIR)
}

pub fn griffr_archives_path(install_root: &Path) -> std::path::PathBuf {
    griffr_path(install_root).join(GRIFFR_ARCHIVES_DIR)
}

pub fn griffr_patch_path(install_root: &Path) -> std::path::PathBuf {
    griffr_path(install_root).join(GRIFFR_PATCH_DIR)
}

pub fn griffr_predownload_path(install_root: &Path) -> std::path::PathBuf {
    griffr_path(install_root).join(GRIFFR_PREDOWNLOAD_DIR)
}

/// Return whether a relative path belongs to Griffr's private install
/// namespace. Comparison is separator- and ASCII-case-insensitive so Windows
/// paths cannot bypass the guard with alternate spelling.
pub fn is_griffr_private_path(path: &Path) -> bool {
    let normalized = normalize_logical_path(&path.to_string_lossy());
    normalized == GRIFFR_DIR
        || normalized
            .strip_prefix(GRIFFR_DIR)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub fn resource_manifest_filename(kind: ResourceManifestKind, resource_name: &str) -> String {
    format!("{}_{}.json", kind.prefix(), resource_name)
}

pub fn resource_manifest_url(
    resource_path: &str,
    kind: ResourceManifestKind,
    resource_name: &str,
) -> String {
    format!(
        "{}/{}",
        resource_path.trim_end_matches('/'),
        resource_manifest_filename(kind, resource_name)
    )
}

pub fn streaming_assets_path(data_root: &Path) -> std::path::PathBuf {
    data_root.join(STREAMING_ASSETS_DIR)
}

pub fn persistent_path(data_root: &Path) -> std::path::PathBuf {
    data_root.join(PERSISTENT_DIR)
}

pub fn vfs_path(root: &Path) -> std::path::PathBuf {
    root.join(VFS_DIR)
}

pub fn normalize_logical_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

pub fn is_resource_baseline_path(path: &str) -> bool {
    let normalized = normalize_logical_path(path);
    normalized.starts_with("streamingassets/vfs/")
        || normalized.contains("/streamingassets/vfs/")
        || normalized == "streamingassets/index_initial.json"
        || normalized == "streamingassets/index_main.json"
        || normalized.ends_with("/streamingassets/index_initial.json")
        || normalized.ends_with("/streamingassets/index_main.json")
}

pub fn is_launcher_metadata_path(path: &str) -> bool {
    matches!(
        normalize_logical_path(path).as_str(),
        CONFIG_INI_NAME
            | GAME_FILES_NAME
            | PACKAGE_FILES_NAME
            | "game-launcher-config.json"
            | "manifest.json"
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        griffr_archives_path, griffr_patch_path, griffr_path, griffr_predownload_path,
        is_griffr_private_path, is_launcher_metadata_path, is_resource_baseline_path,
        normalize_logical_path,
    };

    #[test]
    fn normalize_logical_path_standardizes_separators_and_case() {
        assert_eq!(
            normalize_logical_path(".\\Endfield_Data\\StreamingAssets\\Foo"),
            "endfield_data/streamingassets/foo"
        );
        assert_eq!(normalize_logical_path("/VFS/bar"), "vfs/bar");
    }

    #[test]
    fn resource_baseline_scope_matches_streaming_assets_only() {
        assert!(is_resource_baseline_path(
            "Endfield_Data/StreamingAssets/VFS/file.bin"
        ));
        assert!(is_resource_baseline_path(
            "Endfield_Data/StreamingAssets/index_initial.json"
        ));
        assert!(is_resource_baseline_path("StreamingAssets/index_main.json"));
        assert!(!is_resource_baseline_path(
            "Endfield_Data/Persistent/VFS/file.bin"
        ));
        assert!(!is_resource_baseline_path("Data/VFS/file.bin"));
    }

    #[test]
    fn griffr_paths_share_one_private_root() {
        let install = Path::new("game");
        assert_eq!(griffr_path(install), install.join(".griffr"));
        assert_eq!(
            griffr_archives_path(install),
            install.join(".griffr/archives")
        );
        assert_eq!(griffr_patch_path(install), install.join(".griffr/patch"));
        assert_eq!(
            griffr_predownload_path(install),
            install.join(".griffr/predownload")
        );
    }

    #[test]
    fn private_namespace_match_is_case_and_separator_insensitive() {
        assert!(is_griffr_private_path(Path::new(
            ".GRIFFR\\PATCH\\PLAN.JSON"
        )));
        assert!(is_griffr_private_path(Path::new(
            "./.griffr/predownload/1.0-1.1"
        )));
        assert!(is_griffr_private_path(Path::new(".griffr")));
        assert!(!is_griffr_private_path(Path::new(
            "data/.griffr/state.json"
        )));
        assert!(!is_griffr_private_path(Path::new(
            ".griffr-other/state.json"
        )));
    }

    #[test]
    fn launcher_metadata_path_matches_expected_files_only() {
        assert!(is_launcher_metadata_path("config.ini"));
        assert!(is_launcher_metadata_path("Package_Files"));
        assert!(!is_launcher_metadata_path("Endfield_Data/config.ini"));
    }
}

use std::path::Path;

pub fn normalize_logical_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

pub fn logical_path_from_root(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|rel| normalize_logical_path(&rel.to_string_lossy()))
}

pub fn is_launcher_metadata_path(path: &str) -> bool {
    matches!(
        normalize_logical_path(path).as_str(),
        "config.ini" | "game_files" | "package_files"
    )
}

#[cfg(test)]
mod tests {
    use super::{is_launcher_metadata_path, logical_path_from_root, normalize_logical_path};
    use std::path::Path;

    #[test]
    fn normalize_logical_path_standardizes_separators_and_case() {
        assert_eq!(
            normalize_logical_path(".\\Endfield_Data\\StreamingAssets\\Foo"),
            "endfield_data/streamingassets/foo"
        );
        assert_eq!(normalize_logical_path("/VFS/bar"), "vfs/bar");
    }

    #[test]
    fn logical_path_from_root_returns_normalized_relative_path() {
        let root = Path::new("C:\\Games\\Endfield\\Persistent");
        let path = Path::new("C:\\Games\\Endfield\\Persistent\\VFS\\Foo");
        assert_eq!(
            logical_path_from_root(root, path).as_deref(),
            Some("vfs/foo")
        );
    }

    #[test]
    fn launcher_metadata_path_matches_expected_files_only() {
        assert!(is_launcher_metadata_path("config.ini"));
        assert!(is_launcher_metadata_path("Package_Files"));
        assert!(!is_launcher_metadata_path("Endfield_Data/config.ini"));
    }
}

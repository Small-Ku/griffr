use std::path::Path;

pub const CONFIG_INI_NAME: &str = "config.ini";
pub const GAME_FILES_NAME: &str = "game_files";
pub const PACKAGE_FILES_NAME: &str = "package_files";
pub const PATCH_MANIFEST_NAME: &str = "patch.json";
pub const PATCH_STAGE_DIR: &str = "vfs_files";
pub const PATCH_FILES_STAGE_DIR: &str = "files";
pub const PATCH_DIFF_STAGE_DIR: &str = "vfs_patch";
pub const DELETE_FILES_MANIFEST_NAME: &str = "delete_files.txt";

pub fn launcher_files_base_url(file_path: &str) -> &str {
    let normalized = file_path.trim_end_matches('/');
    if normalized.rsplit('/').next() == Some(GAME_FILES_NAME) {
        &normalized[..normalized.len() - GAME_FILES_NAME.len() - 1]
    } else {
        normalized
    }
}

pub fn launcher_metadata_url(file_path: &str, filename: &str) -> String {
    format!("{}/{}", launcher_files_base_url(file_path), filename)
}

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
        CONFIG_INI_NAME | GAME_FILES_NAME | PACKAGE_FILES_NAME
    )
}

pub fn build_cdn_file_url(base_url: &str, logical_path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let encoded = logical_path
        .replace('\\', "/")
        .split('/')
        .map(percent_encode_path_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("{base}/{encoded}")
}

fn percent_encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for &byte in segment.as_bytes() {
        if is_unreserved_path_byte(byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(nibble_to_hex(byte >> 4));
            encoded.push(nibble_to_hex(byte & 0x0f));
        }
    }
    encoded
}

fn is_unreserved_path_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
    )
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => unreachable!("nibble must be <= 15"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_cdn_file_url, is_launcher_metadata_path, launcher_files_base_url,
        launcher_metadata_url, logical_path_from_root, normalize_logical_path, GAME_FILES_NAME,
    };
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

    #[test]
    fn build_cdn_file_url_encodes_hash_in_path_segment() {
        assert_eq!(
            build_cdn_file_url(
                "https://cdn.example/files",
                "Arknights_Data/StreamingAssets/AB/Windows/arts/dynchars/char_003_kalts_boc#6.ab"
            ),
            "https://cdn.example/files/Arknights_Data/StreamingAssets/AB/Windows/arts/dynchars/char_003_kalts_boc%236.ab"
        );
    }

    #[test]
    fn build_cdn_file_url_normalizes_backslashes_and_encodes_brackets() {
        assert_eq!(
            build_cdn_file_url("https://cdn.example/files/", "ui\\[uc]battlefinish.ab"),
            "https://cdn.example/files/ui/%5Buc%5Dbattlefinish.ab"
        );
    }

    #[test]
    fn launcher_metadata_urls_share_one_base_rule() {
        assert_eq!(
            launcher_files_base_url("https://cdn.example/files/game_files"),
            "https://cdn.example/files"
        );
        assert_eq!(
            launcher_metadata_url("https://cdn.example/files/game_files", GAME_FILES_NAME),
            "https://cdn.example/files/game_files"
        );
    }
}

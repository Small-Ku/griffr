use super::workflow::*;
use griffr_common::api::types::{PackFile, PackageInfo};

#[test]
fn required_install_bytes_uses_larger_of_archives_and_package_total() {
    let pkg = PackageInfo {
        packs: vec![
            PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "8".to_string(),
            },
            PackFile {
                url: "https://example.com/full.zip.002".to_string(),
                md5: "def".to_string(),
                package_size: "7".to_string(),
            },
        ],
        total_size: "20".to_string(),
        file_path: "https://example.com/files".to_string(),
        game_files_md5: None,
    };

    assert_eq!(required_install_bytes(&pkg), 20);
}

#[test]
fn required_install_bytes_falls_back_to_archive_sum_when_total_size_invalid() {
    let pkg = PackageInfo {
        packs: vec![
            PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "4".to_string(),
            },
            PackFile {
                url: "https://example.com/full.zip.002".to_string(),
                md5: "def".to_string(),
                package_size: "6".to_string(),
            },
        ],
        total_size: "invalid".to_string(),
        file_path: "https://example.com/files".to_string(),
        game_files_md5: None,
    };

    assert_eq!(required_install_bytes(&pkg), 10);
}

#[test]
#[ignore = "Uses host filesystem to query real free disk space"]
fn disk_available_bytes_reads_real_disk() {
    let cwd = std::env::current_dir().expect("current dir");
    let available = disk_available_bytes(&cwd).expect("query free space for cwd");
    assert!(available > 0, "expected positive available space");

    // Exercise fallback for not-yet-existing install paths.
    let deep_missing_path = cwd
        .join("griffr-test")
        .join("disk-space")
        .join("missing-target");
    let fallback_available =
        disk_available_bytes(&deep_missing_path).expect("query free space for nested path");
    assert!(
        fallback_available > 0,
        "expected positive available space for nested path"
    );
}

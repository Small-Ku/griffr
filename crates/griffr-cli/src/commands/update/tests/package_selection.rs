use super::*;
#[test]
fn archive_base_name_is_extracted() {
    let url = "https://example.com/path/Beyond_Release_v1d1.zip.001?token=abc";
    let pack = griffr_common::api::types::PackFile {
        url: url.to_string(),
        md5: String::new(),
        package_size: "0".to_string(),
    };
    assert_eq!(pack.archive_base_name(), Some("Beyond_Release_v1d1"));
}

#[test]
fn archive_base_name_supports_single_zip_archives() {
    let url = "https://example.com/path/Beyond_Release_v1d1.zip?token=abc";
    let pack = griffr_common::api::types::PackFile {
        url: url.to_string(),
        md5: String::new(),
        package_size: "0".to_string(),
    };
    assert_eq!(pack.archive_base_name(), Some("Beyond_Release_v1d1"));
}

#[test]
fn archive_base_name_is_extracted_from_non_first_split_part() {
    let url = "https://example.com/path/Beyond_Release_v1d1.zip.002?token=abc";
    let pack = griffr_common::api::types::PackFile {
        url: url.to_string(),
        md5: String::new(),
        package_size: "0".to_string(),
    };
    assert_eq!(pack.archive_base_name(), Some("Beyond_Release_v1d1"));
}

#[test]
fn choose_patch_package_when_available() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1".to_string(),
            }],
            total_size: "1".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: Some("def".to_string()),
        }),
        patch: Some(PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![PackFile {
                url: "https://example.com/patch.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1".to_string(),
            }],
            total_size: "1".to_string(),
            package_size: "1".to_string(),
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    assert_eq!(
        choose_update_package(&response, Some("1.0.13")).unwrap(),
        UpdatePackageKind::Patch
    );
}

#[test]
fn choose_full_package_when_patch_missing() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1".to_string(),
            }],
            total_size: "1".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: Some("def".to_string()),
        }),
        patch: None,
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    assert_eq!(
        choose_update_package(&response, Some("1.0.13")).unwrap(),
        UpdatePackageKind::Full
    );
}

#[test]
fn choose_full_package_when_patch_version_mismatches() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1".to_string(),
            }],
            total_size: "1".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: Some("def".to_string()),
        }),
        patch: Some(PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![PackFile {
                url: "https://example.com/patch.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1".to_string(),
            }],
            total_size: "1".to_string(),
            package_size: "1".to_string(),
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    assert_eq!(
        choose_update_package(&response, Some("1.0.14")).unwrap(),
        UpdatePackageKind::Full
    );
}

#[test]
fn reject_patch_only_update_when_version_mismatches() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: None,
        patch: Some(PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![PackFile {
                url: "https://example.com/patch.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1".to_string(),
            }],
            total_size: "1".to_string(),
            package_size: "1".to_string(),
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    let err = choose_update_package(&response, Some("1.0.14")).unwrap_err();
    assert!(err.to_string().contains("Patch package was returned"));
}

#[test]
fn describe_selection_mentions_patch_match_reason() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![],
            total_size: "0".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: None,
        }),
        patch: Some(PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![PackFile {
                url: "https://example.com/patch.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1".to_string(),
            }],
            total_size: "0".to_string(),
            package_size: "0".to_string(),
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    let msg = describe_update_package_selection(
        &response,
        Some("1.0.13"),
        UpdatePackageKind::Patch,
        false,
    );
    assert!(msg.contains("Using patch package"));
    assert!(msg.contains("matches request_version"));
}

#[test]
fn describe_selection_mentions_full_fallback_when_patch_mismatch() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![],
            total_size: "0".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: None,
        }),
        patch: Some(PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![],
            total_size: "0".to_string(),
            package_size: "0".to_string(),
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    let msg = describe_update_package_selection(
        &response,
        Some("1.0.14"),
        UpdatePackageKind::Full,
        false,
    );
    assert!(msg.contains("Using full package"));
    assert!(msg.contains("does not match"));
}

#[test]
fn describe_selection_mentions_forced_full() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![],
            total_size: "0".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: None,
        }),
        patch: Some(PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![],
            total_size: "0".to_string(),
            package_size: "0".to_string(),
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    let msg =
        describe_update_package_selection(&response, Some("1.0.13"), UpdatePackageKind::Full, true);
    assert!(msg.contains("--full-package"));
}

#[test]
fn selected_archive_plan_uses_patch_parts() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: None,
        patch: Some(PatchInfo {
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
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    let plan = selected_archive_plan(&response, UpdatePackageKind::Patch).unwrap();
    assert_eq!(plan.0, "patch");
    assert_eq!(plan.1, 2);
    assert_eq!(plan.2, 7);
}

#[test]
fn dry_run_plan_includes_verify_and_vfs_steps() {
    let response = GetLatestGameResponse {
        action: 1,
        request_version: "1.0.13".to_string(),
        version: "1.1.9".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "8".to_string(),
            }],
            total_size: "8".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: Some("def".to_string()),
        }),
        patch: None,
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };

    let lines = build_update_dry_run_plan(
        Path::new("C:\\Games\\Endfield"),
        "1.0.13",
        &response,
        UpdatePackageKind::Full,
        &[],
        false,
        None,
        false,
        false,
        false,
        false,
    );

    assert!(lines
        .iter()
        .any(|line| line.contains("Would download full archive parts")));
    assert!(lines
        .iter()
        .any(|line| line.contains("Would run post-update integrity verification")));
    assert!(lines
        .iter()
        .any(|line| line.contains("Would probe the target's launcher resource-index API")));
}

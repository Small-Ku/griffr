use super::*;
use griffr_common::runtime::UpdatePackageKind;

fn response_with_full_and_patch() -> GetLatestGameResponse {
    GetLatestGameResponse {
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
    }
}

#[test]
fn describe_selection_mentions_patch_match_reason() {
    let response = response_with_full_and_patch();
    let message = describe_update_package_selection(
        &response,
        Some("1.0.13"),
        UpdatePackageKind::Patch,
        false,
    );
    assert!(message.contains("Using patch package"));
    assert!(message.contains("matches request_version"));
}

#[test]
fn describe_selection_mentions_full_fallback_when_patch_mismatch() {
    let response = response_with_full_and_patch();
    let message = describe_update_package_selection(
        &response,
        Some("1.0.14"),
        UpdatePackageKind::Full,
        false,
    );
    assert!(message.contains("Using full package"));
    assert!(message.contains("does not match"));
}

#[test]
fn describe_selection_mentions_forced_full() {
    let response = response_with_full_and_patch();
    let message =
        describe_update_package_selection(&response, Some("1.0.13"), UpdatePackageKind::Full, true);
    assert!(message.contains("--full-package"));
}

#[test]
fn dry_run_plan_includes_archive_verify_and_vfs_steps() {
    let response = response_with_full_and_patch();
    let lines = build_update_dry_run_plan(
        Path::new(r"C:\Games\Endfield"),
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

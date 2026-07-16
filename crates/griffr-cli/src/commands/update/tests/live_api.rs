use super::*;
use griffr_common::runtime::{select_update_package, UpdatePackageKind};
#[compio::test]
#[ignore = "Makes real network request"]
async fn real_cn_endfield_patch_and_full_fallback_selection() {
    let api_client = ApiClient::new().expect("Failed to create API client");
    let target = griffr_common::config::resolve_api_target(
        &GameId::ENDFIELD,
        griffr_common::config::RegionId::Cn,
        &ChannelPair::from_api("1", None::<String>).unwrap(),
        &griffr_common::config::ApiTargetOverrides::default(),
    )
    .unwrap();

    // Observed live behavior (2026-04-19):
    // - 1.1.9 returns patch payload for CN official.
    // - 1.2.3 does not return patch payload, so updater must use full fallback.
    let patch_case = api_client
        .get_latest_game(&target, Some("1.1.9"))
        .await
        .expect("get_latest_game failed for patch case");
    assert_eq!(patch_case.request_version, "1.1.9");
    assert!(
        patch_case.has_patch_package(),
        "expected patch payload for request_version=1.1.9"
    );
    assert_eq!(
        select_update_package(&patch_case, Some("1.1.9")).expect("selection failed"),
        UpdatePackageKind::Patch
    );

    let full_case = api_client
        .get_latest_game(&target, Some("1.2.3"))
        .await
        .expect("get_latest_game failed for full fallback case");
    assert_eq!(full_case.request_version, "1.2.3");
    assert!(
        !full_case.has_patch_package(),
        "expected no patch payload for request_version=1.2.3"
    );
    assert_eq!(
        select_update_package(&full_case, Some("1.2.3")).expect("selection failed"),
        UpdatePackageKind::Full
    );
}

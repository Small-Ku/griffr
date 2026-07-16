use super::*;

#[compio::test]
#[ignore = "Makes real network request"]
async fn test_real_endfield_sg_known_versions_return_full_or_patch_payloads() {
    let client = ApiClient::new().expect("Failed to create API client");

    let target = crate::config::resolve_api_target(
        &GameId::ENDFIELD,
        RegionId::Sg,
        &ChannelPair::from_api("6", None::<String>).unwrap(),
        &crate::config::ApiTargetOverrides::default(),
    )
    .unwrap();
    for requested_version in ["1.0.13", "1.0.14", "1.1.9"] {
        let info = client
            .get_latest_game(&target, Some(requested_version))
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "failed get_latest_game for Endfield SG requested_version={}: {err}",
                    requested_version
                )
            });

        assert_eq!(
            info.request_version, requested_version,
            "request_version should echo requested version"
        );
        assert_latest_payload_shape(&info);

        if let Some(patch) = info.patch.as_ref() {
            for part in &patch.patches {
                assert!(
                    part.url.contains("/patches/") || part.url.contains("patch"),
                    "patch part URL should look like patch payload, got {}",
                    part.url
                );
            }
        }

        if let Some(pkg) = info.pkg.as_ref() {
            for pack in &pkg.packs {
                assert!(
                    pack.url.contains("/packs/") || pack.url.contains("pack"),
                    "full package URL should look like package payload, got {}",
                    pack.url
                );
            }
        }
    }
}

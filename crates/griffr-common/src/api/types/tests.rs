use super::*;
use crate::api::protocol::{DEFAULT_LANGUAGE, DEFAULT_PLATFORM, LAUNCHER_SOURCE};
#[test]
fn test_pack_file_parsing() {
    let json = r#"{
        "url": "https://beyond.hycdn.cn/pack.zip.001?auth_key=xxx",
        "md5": "abc123",
        "package_size": "1073741824"
    }"#;

    let pack: PackFile = serde_json::from_str(json).unwrap();
    assert_eq!(pack.size(), 1073741824);
    assert_eq!(pack.filename(), Some("pack.zip.001"));
    assert_eq!(pack.archive_base_name(), Some("pack"));
    assert_eq!(pack.md5, "abc123");
}

#[test]
fn test_get_latest_game_response_helpers() {
    let no_update = GetLatestGameResponse {
        action: 0,
        request_version: "1.0.0".to_string(),
        version: "1.0.0".to_string(),
        pkg: None,
        patch: None,
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };
    assert!(!no_update.has_update());
    assert!(!no_update.is_full_install());
    assert!(!no_update.is_patch());
    assert!(!no_update.has_full_package());
    assert!(!no_update.has_patch_package());
    assert!(!no_update.has_pre_patch_package());

    let full_update = GetLatestGameResponse {
        action: 1,
        request_version: "".to_string(),
        version: "1.1.0".to_string(),
        pkg: Some(PackageInfo {
            packs: vec![PackFile {
                url: "https://example.com/full.zip.001".to_string(),
                md5: "abc".to_string(),
                package_size: "1000".to_string(),
            }],
            total_size: "1000".to_string(),
            file_path: "/files".to_string(),
            game_files_md5: None,
        }),
        patch: None,
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };
    assert!(full_update.has_update());
    assert!(full_update.is_full_install());
    assert!(!full_update.is_patch());
    assert!(full_update.has_full_package());
    assert!(!full_update.has_patch_package());
    assert!(!full_update.has_pre_patch_package());

    let patch_update = GetLatestGameResponse {
        action: 2,
        request_version: "1.0.0".to_string(),
        version: "1.0.1".to_string(),
        pkg: None,
        patch: Some(PatchInfo {
            url: "https://example.com/patch.zip".to_string(),
            md5: "abc123".to_string(),
            file_id: "1".to_string(),
            cd_key: None,
            patches: vec![PackFile {
                url: "https://example.com/patch.zip.001".to_string(),
                md5: "abc123".to_string(),
                package_size: "100".to_string(),
            }],
            total_size: "100".to_string(),
            package_size: "100".to_string(),
        }),
        pre_patch: None,
        state: 0,
        launcher_action: 0,
    };
    assert!(patch_update.has_update());
    assert!(!patch_update.is_full_install());
    assert!(patch_update.is_patch());
    assert!(!patch_update.has_full_package());
    assert!(patch_update.has_patch_package());
    assert!(!patch_update.has_pre_patch_package());
}

#[test]
fn test_patch_response_parsing_with_cd_key() {
    let json = r#"{
        "action": 2,
        "version": "1.3.3",
        "request_version": "1.2.5",
        "pkg": null,
        "patch": {
            "url": "https://example.com/patch.zip",
            "md5": "patch-md5",
            "package_size": "6252042045",
            "total_size": "14020404215",
            "file_id": "1",
            "cd_key": "test-patch-password",
            "patches": [
                {
                    "url": "https://example.com/patch.zip.001",
                    "md5": "part-md5",
                    "package_size": "1073741824"
                }
            ]
        },
        "state": 0,
        "launcher_action": 0,
        "pre_patch": null
    }"#;

    let rsp: GetLatestGameResponse = serde_json::from_str(json).unwrap();
    let patch = rsp.patch.as_ref().unwrap();
    assert_eq!(patch.cd_key.as_deref(), Some("test-patch-password"));
    assert_eq!(patch.patches.len(), 1);
}

#[test]
fn test_pre_patch_response_parsing() {
    let json = r#"{
        "action": 0,
        "version": "1.2.5",
        "request_version": "1.2.5",
        "pkg": {
            "packs": [],
            "total_size": "0",
            "file_path": "https://example.com/files",
            "game_files_md5": "18ba9cd98a55f248db2457c418e1be6d"
        },
        "patch": null,
        "state": 0,
        "launcher_action": 0,
        "pre_patch": {
            "package_size": "6356034359",
            "total_size": "14368056414",
            "version": "1.3.3",
            "patches": [
                {
                    "url": "https://example.com/predownload.zip.001?auth_key=test",
                    "md5": "6569f21ee3567069f03ba372ffc7e09d",
                    "package_size": "1073741824"
                }
            ]
        }
    }"#;

    let rsp: GetLatestGameResponse = serde_json::from_str(json).unwrap();
    assert_eq!(rsp.request_version, "1.2.5");
    assert_eq!(rsp.version, "1.2.5");
    assert!(rsp.has_pre_patch_package());
    let pre_patch = rsp.pre_patch.as_ref().unwrap();
    assert_eq!(pre_patch.version, "1.3.3");
    assert_eq!(pre_patch.package_size, "6356034359");
    assert_eq!(pre_patch.total_size, "14368056414");
    assert_eq!(pre_patch.patches.len(), 1);
    assert_eq!(pre_patch.patches[0].filename(), Some("predownload.zip.001"));
    assert_eq!(
        pre_patch.patches[0].archive_base_name(),
        Some("predownload")
    );
}

#[test]
fn test_common_request_serialization() {
    let req = CommonRequest::new("test_appcode", DEFAULT_LANGUAGE, "1", "1");
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("test_appcode"));
    assert!(json.contains(DEFAULT_LANGUAGE));
    assert!(json.contains(DEFAULT_PLATFORM));
    assert!(json.contains(LAUNCHER_SOURCE));
    assert!(!json.contains("common_req"));
}

#[test]
fn test_banner_response_parsing() {
    let json = r#"{
        "data_version": "v1",
        "banners": [
            {
                "id": "77",
                "url": "https://example.com/banner.png",
                "md5": "abc123",
                "jump_url": "https://example.com/link",
                "need_token": true
            }
        ]
    }"#;

    let rsp: BannerResponse = serde_json::from_str(json).unwrap();
    assert_eq!(rsp.data_version, "v1");
    assert_eq!(rsp.banners.len(), 1);
    assert_eq!(rsp.banners[0].id, "77");
    assert_eq!(rsp.banners[0].url, "https://example.com/banner.png");
    assert_eq!(rsp.banners[0].md5, "abc123");
    assert_eq!(rsp.banners[0].jump_url, "https://example.com/link");
    assert!(rsp.banners[0].need_token);
}

#[test]
fn test_main_bg_image_response_parsing() {
    let json = r#"{
        "data_version": "v1",
        "main_bg_image": {
            "url": "https://example.com/bg.webp",
            "md5": "def456",
            "video_url": ""
        }
    }"#;

    let rsp: MainBgImageResponse = serde_json::from_str(json).unwrap();
    assert_eq!(rsp.data_version, "v1");
    assert_eq!(rsp.main_bg_image.url, "https://example.com/bg.webp");
    assert_eq!(rsp.main_bg_image.md5, "def456");
    assert_eq!(rsp.main_bg_image.video_url, "");
}

#[test]
fn test_sidebar_response_parsing() {
    let json = r#"{
        "data_version": "v1",
        "sidebars": [
            {
                "display_type": "DisplayType_RESERVE",
                "media": "Bilibili",
                "pic": null,
                "jump_url": "https://space.bilibili.com",
                "sidebar_labels": [],
                "grid_info": null,
                "need_token": true
            },
            {
                "display_type": "DisplayType_RESERVE",
                "media": "Weibo",
                "pic": {
                    "url": "https://example.com/icon.png",
                    "md5": "abc123",
                    "description": "Weibo Icon"
                },
                "jump_url": "https://weibo.com",
                "sidebar_labels": [
                    {
                        "content": "Official",
                        "jump_url": "https://weibo.com/official"
                    }
                ],
                "grid_info": null,
                "need_token": false
            }
        ]
    }"#;

    let rsp: SidebarResponse = serde_json::from_str(json).unwrap();
    assert_eq!(rsp.data_version, "v1");
    assert_eq!(rsp.sidebars.len(), 2);
    assert_eq!(rsp.sidebars[0].media, "Bilibili");
    assert!(rsp.sidebars[0].pic.is_none());
    assert!(rsp.sidebars[0].need_token);
    assert_eq!(rsp.sidebars[1].media, "Weibo");
    assert!(rsp.sidebars[1].pic.is_some());
    let pic = rsp.sidebars[1].pic.as_ref().unwrap();
    assert_eq!(pic.description, "Weibo Icon");
    assert_eq!(rsp.sidebars[1].sidebar_labels.len(), 1);
    assert_eq!(rsp.sidebars[1].sidebar_labels[0].content, "Official");
}

#[test]
fn test_announcement_response_parsing() {
    let json = r#"{
        "data_version": "v1",
        "tabs": [
            {
                "tabName": "公告",
                "tab_id": "30",
                "announcements": [
                    {
                        "id": "133",
                        "content": "Update Notice",
                        "jump_url": "https://example.com/news",
                        "start_ts": "1775466000000",
                        "need_token": true
                    }
                ]
            }
        ]
    }"#;

    let rsp: AnnouncementResponse = serde_json::from_str(json).unwrap();
    assert_eq!(rsp.data_version, "v1");
    assert_eq!(rsp.tabs.len(), 1);
    assert_eq!(rsp.tabs[0].tab_name, "公告");
    assert_eq!(rsp.tabs[0].announcements.len(), 1);
    assert_eq!(rsp.tabs[0].announcements[0].id, "133");
    assert_eq!(rsp.tabs[0].announcements[0].content, "Update Notice");
    assert!(rsp.tabs[0].announcements[0].need_token);
}

#[test]
fn test_game_file_entry() {
    let json = r#"{"path":"Endfield.exe","md5":"abc123","size":826424}"#;
    let entry: GameFileEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.path, "Endfield.exe");
    assert_eq!(entry.md5, "abc123");
    assert_eq!(entry.size, 826424);
}

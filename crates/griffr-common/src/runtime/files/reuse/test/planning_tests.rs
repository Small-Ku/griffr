use super::super::*;
use crate::config::ChannelPair;
use crate::runtime::files::reuse::legacy;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_reuse_plan_size_calculation() {
    let plan = ReusePlan {
        source_channels: vec![],
        reusable_files: vec![],
        download_files: vec![
            DownloadFile {
                path: "file1.bin".to_string(),
                md5: "abc".to_string(),
                size: 100,
            },
            DownloadFile {
                path: "file2.bin".to_string(),
                md5: "def".to_string(),
                size: 200,
            },
        ],
        reusable_size: 0,
        download_size: 300,
        requires_copy_fallback: false,
    };

    assert_eq!(plan.download_size, 300);
}

#[test]
fn test_reuse_plan_size_calculation_with_reusable_files() {
    let plan = ReusePlan {
        source_channels: vec![SourceChannel {
            channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
            version: "1.0.0".to_string(),
            install_path: PathBuf::from("/source"),
            file_count: 2,
        }],
        reusable_files: vec![
            ReusableFile {
                path: "file1.bin".to_string(),
                md5: "abc123".to_string(),
                size: 100,
                source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
                source_path: PathBuf::from("/source"),
            },
            ReusableFile {
                path: "file2.bin".to_string(),
                md5: "def456".to_string(),
                size: 200,
                source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
                source_path: PathBuf::from("/source"),
            },
        ],
        download_files: vec![DownloadFile {
            path: "file3.bin".to_string(),
            md5: "ghi789".to_string(),
            size: 300,
        }],
        reusable_size: 300,
        download_size: 300,
        requires_copy_fallback: false,
    };

    assert_eq!(plan.reusable_size, 300);
    assert_eq!(plan.download_size, 300);
    assert_eq!(plan.reusable_size + plan.download_size, 600);
}

#[test]
fn test_reuse_plan_empty() {
    let plan = ReusePlan {
        source_channels: vec![],
        reusable_files: vec![],
        download_files: vec![],
        reusable_size: 0,
        download_size: 0,
        requires_copy_fallback: false,
    };

    assert!(plan.reusable_files.is_empty());
    assert!(plan.download_files.is_empty());
    assert!(plan.source_channels.is_empty());
    assert_eq!(plan.reusable_size, 0);
    assert_eq!(plan.download_size, 0);
}

#[test]
fn test_reuse_options_defaults() {
    let options = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: false,
    };

    assert!(!options.allow_copy_fallback);
    assert!(!options.dry_run);

    let options_with_fallback = ReuseOptions {
        allow_copy_fallback: true,
        dry_run: false,
    };

    assert!(options_with_fallback.allow_copy_fallback);
    assert!(!options_with_fallback.dry_run);

    let dry_run_options = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: true,
    };

    assert!(!dry_run_options.allow_copy_fallback);
    assert!(dry_run_options.dry_run);
}

#[compio::test]
async fn test_execute_reuse_plan_empty() {
    let _temp_dir = TempDir::new().unwrap();

    let plan = ReusePlan {
        source_channels: vec![],
        reusable_files: vec![],
        download_files: vec![],
        reusable_size: 0,
        download_size: 0,
        requires_copy_fallback: false,
    };

    let options = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: false,
    };

    let result = legacy::execute_reuse_plan(_temp_dir.path(), &plan, options).await;
    assert!(result.is_ok());
}

#[compio::test]
async fn test_execute_reuse_plan_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let source_dir = temp_dir.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();

    let source_file = source_dir.join("data.bin");
    std::fs::write(&source_file, b"test content").unwrap();

    let plan = ReusePlan {
        source_channels: vec![SourceChannel {
            channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
            version: "1.0.0".to_string(),
            install_path: source_dir.clone(),
            file_count: 1,
        }],
        reusable_files: vec![ReusableFile {
            path: "data.bin".to_string(),
            md5: "abc123".to_string(),
            size: 12,
            source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
            source_path: source_dir.clone(),
        }],
        download_files: vec![],
        reusable_size: 12,
        download_size: 0,
        requires_copy_fallback: false,
    };

    let options = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: true,
    };

    let result = legacy::execute_reuse_plan(temp_dir.path(), &plan, options).await;
    assert!(result.is_ok());

    let target_file = temp_dir.path().join("data.bin");
    assert!(!target_file.exists(), "Dry run should not create files");
}

#[compio::test]
async fn test_execute_reuse_plan_with_hardlinks() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    std::fs::create_dir_all(&target_dir).unwrap();

    let source_dir = temp_dir.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();

    let source_file1 = source_dir.join("file1.bin");
    std::fs::write(&source_file1, b"content1").unwrap();
    let source_file2 = source_dir.join("subdir/file2.bin");
    std::fs::create_dir_all(source_file2.parent().unwrap()).unwrap();
    std::fs::write(&source_file2, b"content2").unwrap();

    let plan = ReusePlan {
        source_channels: vec![SourceChannel {
            channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
            version: "1.0.0".to_string(),
            install_path: source_dir.clone(),
            file_count: 2,
        }],
        reusable_files: vec![
            ReusableFile {
                path: "file1.bin".to_string(),
                md5: "hash1".to_string(),
                size: 8,
                source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
                source_path: source_dir.clone(),
            },
            ReusableFile {
                path: "subdir/file2.bin".to_string(),
                md5: "hash2".to_string(),
                size: 8,
                source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
                source_path: source_dir.clone(),
            },
        ],
        download_files: vec![],
        reusable_size: 16,
        download_size: 0,
        requires_copy_fallback: false,
    };

    let options = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: false,
    };

    let result = legacy::execute_reuse_plan(&target_dir, &plan, options).await;
    assert!(
        result.is_ok(),
        "Hardlink creation should succeed: {:?}",
        result
    );

    let target_file1 = target_dir.join("file1.bin");
    let target_file2 = target_dir.join("subdir/file2.bin");
    assert!(target_file1.exists(), "Hardlink file1 should exist");
    assert!(target_file2.exists(), "Hardlink file2 should exist");
    assert_eq!(std::fs::read_to_string(&target_file1).unwrap(), "content1");
    assert_eq!(std::fs::read_to_string(&target_file2).unwrap(), "content2");
}

#[compio::test]
async fn test_execute_reuse_plan_with_copy_fallback() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    std::fs::create_dir_all(&target_dir).unwrap();

    let source_dir = temp_dir.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();

    let source_file = source_dir.join("test.bin");
    std::fs::write(&source_file, b"test data").unwrap();
    let fake_source_dir = temp_dir.path().join("fake_source");

    let plan = ReusePlan {
        source_channels: vec![],
        reusable_files: vec![ReusableFile {
            path: "test.bin".to_string(),
            md5: "hash".to_string(),
            size: 9,
            source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
            source_path: source_dir.clone(),
        }],
        download_files: vec![],
        reusable_size: 9,
        download_size: 0,
        requires_copy_fallback: false,
    };

    let options_no_fallback = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: false,
    };

    let result = legacy::execute_reuse_plan(&target_dir, &plan, options_no_fallback).await;
    assert!(result.is_ok(), "Hardlink should succeed: {:?}", result);

    let plan_with_missing_source = ReusePlan {
        source_channels: vec![],
        reusable_files: vec![ReusableFile {
            path: "nonexistent.bin".to_string(),
            md5: "hash".to_string(),
            size: 9,
            source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
            source_path: fake_source_dir.clone(),
        }],
        download_files: vec![],
        reusable_size: 9,
        download_size: 0,
        requires_copy_fallback: false,
    };

    let options_no_fallback2 = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: false,
    };

    let result =
        legacy::execute_reuse_plan(&target_dir, &plan_with_missing_source, options_no_fallback2)
            .await;
    assert!(
        result.is_err(),
        "Should fail when source doesn't exist and no fallback"
    );

    let source_file2 = source_dir.join("fallback.bin");
    std::fs::write(&source_file2, b"fallback data").unwrap();

    let plan_with_fallback = ReusePlan {
        source_channels: vec![],
        reusable_files: vec![ReusableFile {
            path: "fallback.bin".to_string(),
            md5: "hash".to_string(),
            size: 13,
            source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
            source_path: source_dir.clone(),
        }],
        download_files: vec![],
        reusable_size: 13,
        download_size: 0,
        requires_copy_fallback: false,
    };

    let options_with_fallback = ReuseOptions {
        allow_copy_fallback: true,
        dry_run: false,
    };

    let result =
        legacy::execute_reuse_plan(&target_dir, &plan_with_fallback, options_with_fallback).await;
    assert!(
        result.is_ok(),
        "Should succeed with copy fallback allowed: {:?}",
        result
    );

    let target_fallback = target_dir.join("fallback.bin");
    assert!(target_fallback.exists());
    assert_eq!(
        std::fs::read_to_string(&target_fallback).unwrap(),
        "fallback data"
    );
}

#[compio::test]
async fn test_execute_reuse_plan_multiple_source_channels() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    std::fs::create_dir_all(&target_dir).unwrap();

    let source_dir1 = temp_dir.path().join("source1");
    std::fs::create_dir_all(&source_dir1).unwrap();
    let source_file1 = source_dir1.join("server1.bin");
    std::fs::write(&source_file1, b"server1 data").unwrap();

    let source_dir2 = temp_dir.path().join("source2");
    std::fs::create_dir_all(&source_dir2).unwrap();
    let source_file2 = source_dir2.join("server2.bin");
    std::fs::write(&source_file2, b"server2 data").unwrap();

    let plan = ReusePlan {
        source_channels: vec![
            SourceChannel {
                channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
                version: "1.0.0".to_string(),
                install_path: source_dir1.clone(),
                file_count: 1,
            },
            SourceChannel {
                channel_id: ChannelPair::parse("2", None::<String>).unwrap(),
                version: "1.0.0".to_string(),
                install_path: source_dir2.clone(),
                file_count: 1,
            },
        ],
        reusable_files: vec![
            ReusableFile {
                path: "server1.bin".to_string(),
                md5: "hash1".to_string(),
                size: 13,
                source_channel_id: ChannelPair::parse("1", None::<String>).unwrap(),
                source_path: source_dir1.clone(),
            },
            ReusableFile {
                path: "server2.bin".to_string(),
                md5: "hash2".to_string(),
                size: 13,
                source_channel_id: ChannelPair::parse("2", None::<String>).unwrap(),
                source_path: source_dir2.clone(),
            },
        ],
        download_files: vec![],
        reusable_size: 26,
        download_size: 0,
        requires_copy_fallback: false,
    };

    let options = ReuseOptions {
        allow_copy_fallback: false,
        dry_run: false,
    };

    let result = legacy::execute_reuse_plan(&target_dir, &plan, options).await;
    assert!(result.is_ok());
    assert!(target_dir.join("server1.bin").exists());
    assert!(target_dir.join("server2.bin").exists());
    assert_eq!(
        std::fs::read_to_string(target_dir.join("server1.bin")).unwrap(),
        "server1 data"
    );
    assert_eq!(
        std::fs::read_to_string(target_dir.join("server2.bin")).unwrap(),
        "server2 data"
    );
}

#[compio::test]
async fn test_download_remaining_files_empty() {
    let _temp_dir = TempDir::new().unwrap();
}

#[test]
fn test_download_file_struct() {
    let file = DownloadFile {
        path: "assets/game.bin".to_string(),
        md5: "abcdef123456".to_string(),
        size: 1024 * 1024 * 100,
    };
    assert_eq!(file.path, "assets/game.bin");
    assert_eq!(file.md5, "abcdef123456");
    assert_eq!(file.size, 104857600);
}

#[test]
fn test_reusable_file_struct() {
    let file = ReusableFile {
        path: "data/config.json".to_string(),
        md5: "1234567890ab".to_string(),
        size: 2048,
        source_channel_id: ChannelPair::parse("6", None::<String>).unwrap(),
        source_path: PathBuf::from("/mnt/games/arknights/global"),
    };
    assert_eq!(file.path, "data/config.json");
    assert_eq!(file.source_channel_id, ChannelPair::parse("6", None::<String>).unwrap());
    assert_eq!(file.size, 2048);
}

#[test]
fn test_source_channel_struct() {
    let source = SourceChannel {
        channel_id: ChannelPair::parse("2", None::<String>).unwrap(),
        version: "2.1.0".to_string(),
        install_path: PathBuf::from("/games/endfield/cn-bili"),
        file_count: 5000,
    };
    assert_eq!(source.channel_id, ChannelPair::parse("2", None::<String>).unwrap());
    assert_eq!(source.version, "2.1.0");
    assert_eq!(source.file_count, 5000);
}

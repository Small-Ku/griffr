use super::*;

#[test]
fn clap_accepts_native_region_defaults_and_sub_channel_alias() {
    let cli = Cli::try_parse_from([
        "griffr",
        "install",
        "--game",
        "endfield",
        "--region",
        "sg",
        "--sub_channel",
        "gplay",
        "--path",
        r"C:\Games\Endfield",
    ])
    .unwrap();
    let Commands::Install { remote, .. } = cli.command else {
        panic!("expected install command");
    };

    let (game, region, channel) = parse_remote_args(remote).unwrap();
    assert_eq!(game, GameId::ENDFIELD);
    assert_eq!(region, RegionId::Sg);
    assert_eq!(channel.channel().as_str(), "6");
    assert_eq!(channel.sub_channel().as_str(), "802");
}

#[test]
fn remote_args_use_native_region_and_scoped_aliases() {
    let remote = RequiredGameRegionChannelArgs {
        game: "endfield".to_string(),
        region: "sg".to_string(),
        channel: None,
        sub_channel: Some("google-play".to_string()),
    };

    let (game, region, channel) = parse_remote_args(remote).unwrap();
    assert_eq!(game, GameId::ENDFIELD);
    assert_eq!(region, RegionId::Sg);
    assert_eq!(channel.channel().as_str(), "6");
    assert_eq!(channel.sub_channel().as_str(), "802");
}

#[test]
fn remote_parser_does_not_reject_arknights_sg_combination() {
    let remote = RequiredGameRegionChannelArgs {
        game: "arknights".to_string(),
        region: "sg".to_string(),
        channel: None,
        sub_channel: None,
    };

    let (game, region, channel) = parse_remote_args(remote).unwrap();
    assert_eq!(game, GameId::ARKNIGHTS);
    assert_eq!(region, RegionId::Sg);
    assert_eq!(channel.channel().as_str(), "6");
    assert_eq!(channel.sub_channel().as_str(), "6");
}

#[test]
fn remote_args_default_to_region_official_channel() {
    let remote = RequiredGameRegionChannelArgs {
        game: "endfield".to_string(),
        region: "cn".to_string(),
        channel: None,
        sub_channel: None,
    };

    let (_, region, channel) = parse_remote_args(remote).unwrap();
    assert_eq!(region, RegionId::Cn);
    assert_eq!(channel.channel().as_str(), "1");
    assert_eq!(channel.sub_channel().as_str(), "1");
}

#[test]
fn clap_accepts_explicit_volume_policy_and_reuse_step_tuning() {
    let cli = Cli::try_parse_from([
        "griffr",
        "--volume-read-limit",
        "3",
        "--volume-write-limit",
        "1",
        "--volume-metadata-limit",
        "2",
        "--volume-streaming-pressure-limit",
        "4",
        "--volume-streaming-mode",
        "exclusive",
        "--reuse-queue-limit",
        "24",
        "verify",
        "--path",
        r"C:\Games\Endfield",
    ])
    .unwrap();

    assert_eq!(cli.volume_read_limit, 3);
    assert_eq!(cli.volume_write_limit, 1);
    assert_eq!(cli.volume_metadata_limit, 2);
    assert_eq!(cli.volume_streaming_pressure_limit, 4);
    assert_eq!(cli.volume_streaming_mode, VolumeStreamingModeArg::Exclusive);
    assert_eq!(cli.reuse_queue_limit, 24);
}

#[test]
fn clap_volume_policy_defaults_match_common_nvme_parameters() {
    let cli = Cli::try_parse_from(["griffr", "verify", "--path", r"C:\Games\Endfield"]).unwrap();

    assert_eq!(
        cli.volume_read_limit,
        griffr_common::runtime::task_pool::DEFAULT_VOLUME_READ_LIMIT
    );
    assert_eq!(
        cli.volume_write_limit,
        griffr_common::runtime::task_pool::DEFAULT_VOLUME_WRITE_LIMIT
    );
    assert_eq!(
        cli.volume_metadata_limit,
        griffr_common::runtime::task_pool::DEFAULT_VOLUME_METADATA_LIMIT
    );
    assert_eq!(
        cli.volume_streaming_pressure_limit,
        griffr_common::runtime::task_pool::DEFAULT_VOLUME_STREAMING_PRESSURE_LIMIT
    );
    assert_eq!(cli.volume_streaming_mode, VolumeStreamingModeArg::Mixed);
}

#[test]
fn update_accepts_repeated_target_paths() {
    let cli = Cli::try_parse_from([
        "griffr",
        "update",
        "--path",
        r"C:\Games\Endfield-CN",
        "--path",
        r"C:\Games\Endfield-OS",
    ])
    .unwrap();
    let Commands::Update { paths, .. } = cli.command else {
        panic!("expected update command");
    };

    assert_eq!(paths.paths.len(), 2);
}

#[test]
fn verify_batch_peers_can_satisfy_relink_parse_requirements() {
    let cli = Cli::try_parse_from([
        "griffr",
        "verify",
        "--path",
        r"C:\Games\Endfield-CN",
        "--path",
        r"C:\Games\Endfield-OS",
        "--repair",
        "--relink-reuse",
    ])
    .unwrap();
    let Commands::Verify {
        paths,
        repair,
        relink_reuse,
        ..
    } = cli.command
    else {
        panic!("expected verify command");
    };

    assert_eq!(paths.paths.len(), 2);
    assert!(repair);
    assert!(relink_reuse);
}

#[test]
fn skip_vfs_keeps_install_and_verify_cli_parity() {
    let install = Cli::try_parse_from([
        "griffr",
        "install",
        "--game",
        "endfield",
        "--region",
        "sg",
        "--path",
        r"C:\Games\Endfield",
        "--skip-vfs",
    ])
    .unwrap();
    let Commands::Install {
        resource_policy,
        skip_vfs,
        ..
    } = install.command
    else {
        panic!("expected install command");
    };
    assert!(resource_policy.is_none());
    assert!(skip_vfs);
    assert_eq!(
        ResourcePolicyArg::resolve(resource_policy, skip_vfs),
        ResourcePolicyArg::PackageOnly
    );

    let verify = Cli::try_parse_from([
        "griffr",
        "verify",
        "--path",
        r"C:\Games\Endfield",
        "--skip-vfs",
    ])
    .unwrap();
    let Commands::Verify {
        scope, skip_vfs, ..
    } = verify.command
    else {
        panic!("expected verify command");
    };
    assert!(scope.is_none());
    assert!(skip_vfs);
}

#[test]
fn explicit_resource_policy_conflicts_with_skip_vfs() {
    let Err(error) = Cli::try_parse_from([
        "griffr",
        "update",
        "--path",
        r"C:\Games\Endfield",
        "--resource-policy",
        "auto",
        "--skip-vfs",
    ]) else {
        panic!("expected argument conflict error");
    };

    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

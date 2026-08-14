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
    let Commands::Install { remote, .. } = *cli.command else {
        panic!("expected install command");
    };

    let RemoteTarget::Hypergryph {
        game,
        region,
        channels,
    } = parse_remote_args(remote).unwrap()
    else {
        panic!("expected Hypergryph target");
    };
    assert_eq!(game, GameId::ENDFIELD);
    assert_eq!(region, RegionId::Sg);
    assert_eq!(channels.channel().as_str(), "6");
    assert_eq!(channels.sub_channel().as_str(), "802");
}

#[test]
fn remote_args_use_native_region_and_scoped_aliases() {
    let remote = RequiredGameRegionChannelArgs {
        game: "endfield".to_string(),
        region: "sg".to_string(),
        channel: None,
        sub_channel: Some("google-play".to_string()),
    };

    let RemoteTarget::Hypergryph {
        game,
        region,
        channels,
    } = parse_remote_args(remote).unwrap()
    else {
        panic!("expected Hypergryph target");
    };
    assert_eq!(game, GameId::ENDFIELD);
    assert_eq!(region, RegionId::Sg);
    assert_eq!(channels.channel().as_str(), "6");
    assert_eq!(channels.sub_channel().as_str(), "802");
}

#[test]
fn remote_parser_does_not_reject_arknights_sg_combination() {
    let remote = RequiredGameRegionChannelArgs {
        game: "arknights".to_string(),
        region: "sg".to_string(),
        channel: None,
        sub_channel: None,
    };

    let RemoteTarget::Hypergryph {
        game,
        region,
        channels,
    } = parse_remote_args(remote).unwrap()
    else {
        panic!("expected Hypergryph target");
    };
    assert_eq!(game, GameId::ARKNIGHTS);
    assert_eq!(region, RegionId::Sg);
    assert_eq!(channels.channel().as_str(), "6");
    assert_eq!(channels.sub_channel().as_str(), "6");
}

#[test]
fn remote_args_default_to_region_official_channel() {
    let remote = RequiredGameRegionChannelArgs {
        game: "endfield".to_string(),
        region: "cn".to_string(),
        channel: None,
        sub_channel: None,
    };

    let RemoteTarget::Hypergryph {
        region, channels, ..
    } = parse_remote_args(remote).unwrap()
    else {
        panic!("expected Hypergryph target");
    };
    assert_eq!(region, RegionId::Cn);
    assert_eq!(channels.channel().as_str(), "1");
    assert_eq!(channels.sub_channel().as_str(), "1");
}

#[test]
fn scheduler_tuning_is_not_part_of_the_public_cli() {
    let Err(error) = Cli::try_parse_from([
        "griffr",
        "--volume-read-limit",
        "3",
        "verify",
        "--path",
        r"C:\Games\Endfield",
    ]) else {
        panic!("expected removed scheduler option to be rejected");
    };

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn output_format_is_scoped_to_report_commands() {
    let verify = Cli::try_parse_from([
        "griffr",
        "verify",
        "--path",
        r"C:\Games\Endfield",
        "--output",
        "json",
    ])
    .unwrap();
    let Commands::Verify { report, .. } = *verify.command else {
        panic!("expected verify command");
    };
    assert_eq!(report.output, OutputFormat::Json);

    let Err(error) = Cli::try_parse_from([
        "griffr",
        "update",
        "--path",
        r"C:\Games\Endfield",
        "--output",
        "json",
    ]) else {
        panic!("expected report output to be rejected by update");
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
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
    let Commands::Update { paths, .. } = *cli.command else {
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
    } = *cli.command
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
    } = *install.command
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
    } = *verify.command
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

#[test]
fn launch_accepts_wine_runner_and_prefix() {
    let cli = Cli::try_parse_from([
        "griffr",
        "launch",
        "--path",
        "/games/endfield",
        "--wine",
        "/opt/wine/bin/wine64",
        "--wine-prefix",
        "/home/user/.wine-endfield",
        "--force",
    ])
    .unwrap();

    let Commands::Launch {
        path,
        force,
        wine,
        wine_prefix,
        ..
    } = *cli.command
    else {
        panic!("expected launch command");
    };

    assert_eq!(path, std::path::PathBuf::from("/games/endfield"));
    assert!(force);
    assert_eq!(wine, Some(std::path::PathBuf::from("/opt/wine/bin/wine64")));
    assert_eq!(
        wine_prefix,
        Some(std::path::PathBuf::from("/home/user/.wine-endfield"))
    );
}

#[test]
fn stage_recover_and_resource_commands_use_result_oriented_names() {
    let stage = Cli::try_parse_from([
        "griffr",
        "stage",
        "fetch",
        "--path",
        r"C:\Games\Endfield",
        "--stage-dir",
        r"D:\Staging\Endfield",
    ])
    .unwrap();
    let Commands::Stage { command } = *stage.command else {
        panic!("expected stage command");
    };
    let StageCommands::Fetch { stage_dir, .. } = command else {
        panic!("expected stage fetch command");
    };
    assert_eq!(
        stage_dir,
        Some(std::path::PathBuf::from(r"D:\Staging\Endfield"))
    );

    let recover =
        Cli::try_parse_from(["griffr", "recover", "--path", r"C:\Games\Endfield"]).unwrap();
    assert!(matches!(*recover.command, Commands::Recover { .. }));

    let resources = Cli::try_parse_from([
        "griffr",
        "resources",
        "sync",
        "--path",
        r"C:\Games\Endfield",
        "--allow-download",
    ])
    .unwrap();
    let Commands::Resources { command } = *resources.command else {
        panic!("expected resources command");
    };
    assert!(matches!(command, ResourceCommands::Sync { .. }));
}

#[test]
fn update_stage_contract_is_explicit() {
    let cli = Cli::try_parse_from([
        "griffr",
        "update",
        "--path",
        r"C:\Games\Endfield",
        "--stage-dir",
        r"D:\Staging\Endfield",
        "--require-staged",
    ])
    .unwrap();
    let Commands::Update {
        stage_dir,
        require_staged,
        ..
    } = *cli.command
    else {
        panic!("expected update command");
    };
    assert_eq!(
        stage_dir,
        Some(std::path::PathBuf::from(r"D:\Staging\Endfield"))
    );
    assert!(require_staged);

    let Err(error) = Cli::try_parse_from([
        "griffr",
        "update",
        "--path",
        r"C:\Games\Endfield",
        "--require-staged",
    ]) else {
        panic!("expected --require-staged to require --stage-dir");
    };
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

#[test]
fn dry_run_is_scoped_to_mutating_commands() {
    let install = Cli::try_parse_from([
        "griffr",
        "install",
        "--dry-run",
        "--game",
        "endfield",
        "--region",
        "sg",
        "--path",
        r"C:\Games\Endfield",
    ])
    .unwrap();
    let Commands::Install { mutation, .. } = *install.command else {
        panic!("expected install command");
    };
    assert!(mutation.dry_run);

    let verify = Cli::try_parse_from([
        "griffr",
        "verify",
        "--dry-run",
        "--repair",
        "--path",
        r"C:\Games\Endfield",
    ])
    .unwrap();
    let Commands::Verify { mutation, .. } = *verify.command else {
        panic!("expected verify command");
    };
    assert!(mutation.dry_run);

    let Err(error) = Cli::try_parse_from([
        "griffr",
        "info",
        "--dry-run",
        "--path",
        r"C:\Games\Endfield",
    ]) else {
        panic!("expected info to reject mutation-only --dry-run");
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn batch_controls_default_to_keep_going_and_bound_jobs() {
    let cli = Cli::try_parse_from([
        "griffr",
        "verify",
        "--path",
        r"C:\Games\Endfield-CN",
        "--path",
        r"D:\Games\Endfield-OS",
        "--jobs",
        "2",
        "--keep-going",
    ])
    .unwrap();
    let Commands::Verify { batch, .. } = *cli.command else {
        panic!("expected verify command");
    };
    assert_eq!(batch.jobs, 2);
    assert!(batch.continue_after_failure());

    let cli = Cli::try_parse_from([
        "griffr",
        "update",
        "--path",
        r"C:\Games\Endfield",
        "--fail-fast",
    ])
    .unwrap();
    let Commands::Update { batch, .. } = *cli.command else {
        panic!("expected update command");
    };
    assert!(batch.fail_fast);
    assert!(!batch.continue_after_failure());
}

#[test]
fn batch_targets_keep_full_disjoint_volume_policy() {
    let options = GlobalOptions::from_environment(false, false, OutputFormat::Text);
    let base = options.task_pool_config();
    let batch = options.task_pool_config_for_batch(options.batch_parallelism_limit());

    assert_eq!(batch.default_volume_policy, base.default_volume_policy);
    assert!(options.batch_parallelism_limit() <= base.cpu_slots);
    assert!(options.batch_parallelism_limit() <= base.blocking_slots);
    assert!(options.batch_parallelism_limit() <= base.network_slots);
}

#[test]
fn batch_task_pool_configs_keep_required_blocking_headroom() {
    let options = GlobalOptions::from_environment(false, false, OutputFormat::Text);
    for jobs in [1, 2, 3, 8, 16] {
        let (group, config) = options
            .task_pool_batch(jobs)
            .unwrap_or_else(|error| panic!("invalid shared task pool for {jobs} jobs: {error}"));
        group
            .runner(config)
            .unwrap_or_else(|error| panic!("invalid task-pool config for {jobs} jobs: {error}"));
    }
}

#[test]
fn quiet_is_global_and_conflicts_with_verbose() {
    let cli =
        Cli::try_parse_from(["griffr", "--quiet", "info", "--path", r"C:\Games\Endfield"]).unwrap();
    assert!(cli.quiet);

    let Err(error) = Cli::try_parse_from([
        "griffr",
        "--quiet",
        "--verbose",
        "info",
        "--path",
        r"C:\Games\Endfield",
    ]) else {
        panic!("expected quiet and verbose to conflict");
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

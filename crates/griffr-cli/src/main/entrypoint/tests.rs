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

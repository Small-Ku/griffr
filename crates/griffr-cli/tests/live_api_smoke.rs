use std::env;
use std::ffi::{OsStr, OsString};
use std::process::{Command, Output};

#[path = "test_support/exe.rs"]
mod exe;
use exe::griffr_exe;

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect::<Vec<_>>();
    println!(
        "$ griffr {}",
        args.iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let output = Command::new(griffr_exe())
        .args(&args)
        .env("NO_COLOR", "1")
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("run griffr live smoke command");
    assert!(
        output.status.success(),
        "griffr live smoke failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn hypergryph_target_args(game: &str, region: &str, channel: &str) -> Vec<OsString> {
    let mut args = vec![
        "--game".into(),
        game.into(),
        "--region".into(),
        region.into(),
    ];
    if !channel.is_empty() {
        args.extend(["--channel".into(), channel.into()]);
    }
    args
}

#[test]
#[ignore = "makes read-only requests to production launcher services"]
fn official_api_smoke() {
    let game = required_env("GRIFFR_LIVE_SMOKE_GAME");
    let region = required_env("GRIFFR_LIVE_SMOKE_REGION");
    let channel = env::var("GRIFFR_LIVE_SMOKE_CHANNEL").unwrap_or_default();

    if matches!(region.as_str(), "en" | "jp" | "kr") {
        assert_eq!(
            game, "arknights",
            "YoStar smoke currently covers Arknights only"
        );
        for action in ["config", "cdn", "manifest"] {
            run(["debug", "yostar", action, "--region", region.as_str()]);
        }
        return;
    }

    let target = hypergryph_target_args(&game, &region, &channel);

    let mut news = vec![OsString::from("news")];
    news.extend(target.clone());
    run(news);

    for action in ["get-raw-latest-game", "list-game-files"] {
        let mut args = vec![OsString::from("debug"), OsString::from(action)];
        args.extend(target.clone());
        run(args);
    }

    // Arknights CN is known not to expose the Endfield resource API. Keep the
    // smoke contract provider-correct instead of treating its INVALID_PARAM as
    // an outage.
    if game == "endfield" {
        let mut args = vec![
            OsString::from("debug"),
            OsString::from("get-raw-latest-resources"),
        ];
        args.extend(target);
        run(args);
    }
}

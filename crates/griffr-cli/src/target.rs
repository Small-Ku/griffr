use anyhow::Result;
use griffr_core::{BackendKind, ChannelPair, GameId, GameTarget, RegionId};

pub(crate) type RemoteTarget = GameTarget;

/// Parse raw CLI target fields into the provider-correct core identity.
pub(crate) fn parse_remote_target(
    game: String,
    region: String,
    channel: Option<String>,
    sub_channel: Option<String>,
) -> Result<RemoteTarget> {
    let game = game.parse::<GameId>()?;
    let region = region.parse::<RegionId>()?;
    match region.backend() {
        BackendKind::Hypergryph => Ok(GameTarget::hypergryph(
            game,
            region,
            ChannelPair::parse(region, channel, sub_channel)?,
        )?),
        BackendKind::Yostar => {
            if channel.is_some() || sub_channel.is_some() {
                anyhow::bail!(
                    "YoStar {region} does not expose launcher channel/sub-channel IDs; omit --channel and --sub-channel"
                );
            }
            Ok(GameTarget::yostar(game, region)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hypergryph_target_owns_channels() {
        let target = parse_remote_target("endfield".into(), "sg".into(), None, None).unwrap();
        let RemoteTarget::Hypergryph { channels, .. } = target else {
            panic!("expected Hypergryph target");
        };
        assert_eq!(channels.channel().as_str(), "6");
        assert_eq!(channels.sub_channel().as_str(), "6");
    }

    #[test]
    fn yostar_target_has_no_fake_channel_pair() {
        let target = parse_remote_target("arknights".into(), "jp".into(), None, None).unwrap();
        assert!(matches!(target, RemoteTarget::Yostar { .. }));
    }

    #[test]
    fn yostar_rejects_hypergryph_fields_and_unknown_games() {
        assert!(parse_remote_target(
            "arknights".into(),
            "en".into(),
            Some("official".into()),
            None,
        )
        .is_err());
        assert!(parse_remote_target("endfield".into(), "jp".into(), None, None).is_err());
    }
}

use anyhow::{Context, Result};
use griffr_core::{BackendKind, ChannelPair, GameId, RegionId};

/// Provider-correct remote target parsed once at the CLI boundary.
///
/// YoStar deliberately has no channel fields; Hypergryph/Gryphline always do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteTarget {
    Hypergryph {
        game: GameId,
        region: RegionId,
        channels: ChannelPair,
    },
    Yostar {
        game: GameId,
        region: RegionId,
    },
}

impl RemoteTarget {
    pub(crate) fn parse(
        game: String,
        region: String,
        channel: Option<String>,
        sub_channel: Option<String>,
    ) -> Result<Self> {
        let game = game.parse::<GameId>()?;
        let region = region.parse::<RegionId>()?;
        Self::from_parts(game, region, channel, sub_channel)
    }

    pub(crate) fn from_detected(
        game: GameId,
        region: RegionId,
        channels: Option<ChannelPair>,
    ) -> Result<Self> {
        match region.backend() {
            BackendKind::Hypergryph => Ok(Self::Hypergryph {
                game,
                region,
                channels: channels
                    .context("Hypergryph/Gryphline target is missing detected channel metadata")?,
            }),
            BackendKind::Yostar => {
                if channels.is_some() {
                    anyhow::bail!("YoStar target unexpectedly contains launcher channel metadata");
                }
                Self::validate_yostar_game(&game, region)?;
                Ok(Self::Yostar { game, region })
            }
        }
    }

    fn from_parts(
        game: GameId,
        region: RegionId,
        channel: Option<String>,
        sub_channel: Option<String>,
    ) -> Result<Self> {
        match region.backend() {
            BackendKind::Hypergryph => Ok(Self::Hypergryph {
                channels: ChannelPair::parse(region, channel, sub_channel)?,
                game,
                region,
            }),
            BackendKind::Yostar => {
                if channel.is_some() || sub_channel.is_some() {
                    anyhow::bail!(
                        "YoStar {region} does not expose launcher channel/sub-channel IDs; omit --channel and --sub-channel"
                    );
                }
                Self::validate_yostar_game(&game, region)?;
                Ok(Self::Yostar { game, region })
            }
        }
    }

    fn validate_yostar_game(game: &GameId, region: RegionId) -> Result<()> {
        if game != &GameId::ARKNIGHTS {
            anyhow::bail!(
                "YoStar backend supports Arknights regions kr, en, and jp; got --game {game} --region {region}"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hypergryph_target_owns_channels() {
        let target = RemoteTarget::parse("endfield".into(), "sg".into(), None, None).unwrap();
        let RemoteTarget::Hypergryph { channels, .. } = target else {
            panic!("expected Hypergryph target");
        };
        assert_eq!(channels.channel().as_str(), "6");
        assert_eq!(channels.sub_channel().as_str(), "6");
    }

    #[test]
    fn yostar_target_has_no_fake_channel_pair() {
        let target = RemoteTarget::parse("arknights".into(), "jp".into(), None, None).unwrap();
        assert!(matches!(target, RemoteTarget::Yostar { .. }));
    }

    #[test]
    fn yostar_rejects_hypergryph_fields_and_unknown_games() {
        assert!(RemoteTarget::parse(
            "arknights".into(),
            "en".into(),
            Some("official".into()),
            None,
        )
        .is_err());
        assert!(RemoteTarget::parse("endfield".into(), "jp".into(), None, None).is_err());
    }
}

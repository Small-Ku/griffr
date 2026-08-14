use serde::{Deserialize, Serialize};

use crate::{BackendKind, ChannelPair, Error, GameId, RegionId, Result};

/// Provider-correct identity for one supported remote game deployment.
///
/// Hypergryph/Gryphline deployments carry native launcher channel metadata;
/// YoStar deployments deliberately do not. Keeping that distinction in the
/// type prevents callers and persisted state from manufacturing placeholder
/// channel IDs for providers that have no such concept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum GameTarget {
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

impl GameTarget {
    pub fn hypergryph(game: GameId, region: RegionId, channels: ChannelPair) -> Result<Self> {
        let target = Self::Hypergryph {
            game,
            region,
            channels,
        };
        target.validate()?;
        Ok(target)
    }

    pub fn yostar(game: GameId, region: RegionId) -> Result<Self> {
        let target = Self::Yostar { game, region };
        target.validate()?;
        Ok(target)
    }

    /// Construct a target from already-detected provider metadata.
    pub fn from_detected(
        game: GameId,
        region: RegionId,
        channels: Option<ChannelPair>,
    ) -> Result<Self> {
        match region.backend() {
            BackendKind::Hypergryph => Self::hypergryph(
                game,
                region,
                channels.ok_or_else(|| Error::Message {
                    context: "Configuration error: ",
                    detail: "Hypergryph/Gryphline target is missing detected channel metadata"
                        .to_string(),
                })?,
            ),
            BackendKind::Yostar => {
                if channels.is_some() {
                    return Err(Error::Message {
                        context: "Configuration error: ",
                        detail: "YoStar target unexpectedly contains launcher channel metadata"
                            .to_string(),
                    });
                }
                Self::yostar(game, region)
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Hypergryph { region, .. } if region.backend() != BackendKind::Hypergryph => {
                Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!(
                        "Hypergryph target cannot use YoStar deployment region {region}"
                    ),
                })
            }
            Self::Yostar { region, .. } if region.backend() != BackendKind::Yostar => {
                Err(Error::Message {
                    context: "Configuration error: ",
                    detail: format!("YoStar target cannot use Hypergryph deployment region {region}"),
                })
            }
            Self::Yostar { game, region } if game != &GameId::ARKNIGHTS => Err(Error::Message {
                context: "Configuration error: ",
                detail: format!(
                    "YoStar backend supports Arknights regions kr, en, and jp; got game={game} region={region}"
                ),
            }),
            _ => Ok(()),
        }
    }

    pub fn game(&self) -> &GameId {
        match self {
            Self::Hypergryph { game, .. } | Self::Yostar { game, .. } => game,
        }
    }

    pub const fn region(&self) -> RegionId {
        match self {
            Self::Hypergryph { region, .. } | Self::Yostar { region, .. } => *region,
        }
    }

    pub const fn backend(&self) -> BackendKind {
        match self {
            Self::Hypergryph { .. } => BackendKind::Hypergryph,
            Self::Yostar { .. } => BackendKind::Yostar,
        }
    }

    pub fn channels(&self) -> Option<&ChannelPair> {
        match self {
            Self::Hypergryph { channels, .. } => Some(channels),
            Self::Yostar { .. } => None,
        }
    }
}

impl std::fmt::Display for GameTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hypergryph {
                game,
                region,
                channels,
            } => write!(
                f,
                "{game}/{region}/{}/{}",
                channels.channel(),
                channels.sub_channel()
            ),
            Self::Yostar { game, region } => write!(f, "{game}/{region}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hypergryph_target_requires_hypergryph_region() {
        let channels = ChannelPair::parse(RegionId::Cn, None, None).unwrap();
        let target = GameTarget::hypergryph(GameId::ENDFIELD, RegionId::Cn, channels).unwrap();
        assert_eq!(target.backend(), BackendKind::Hypergryph);
        assert!(target.channels().is_some());

        let channels = ChannelPair::from_api("1", None::<String>).unwrap();
        assert!(GameTarget::hypergryph(GameId::ENDFIELD, RegionId::Jp, channels).is_err());
    }

    #[test]
    fn yostar_target_has_no_channel_identity() {
        let target = GameTarget::yostar(GameId::ARKNIGHTS, RegionId::Jp).unwrap();
        assert_eq!(target.to_string(), "arknights/jp");
        assert!(target.channels().is_none());
        assert!(GameTarget::yostar(GameId::ENDFIELD, RegionId::Jp).is_err());
        assert!(GameTarget::yostar(GameId::ARKNIGHTS, RegionId::Cn).is_err());
    }

    #[test]
    fn serialized_shape_is_provider_specific() {
        let target = GameTarget::yostar(GameId::ARKNIGHTS, RegionId::En).unwrap();
        let value = serde_json::to_value(&target).unwrap();
        assert_eq!(value["backend"], "yostar");
        assert_eq!(value["game"], "arknights");
        assert_eq!(value["region"], "en");
        assert!(value.get("channels").is_none());
    }
}

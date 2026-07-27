use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::config::GameId;
use griffr_common::runtime::{detect_local_install, LocalInstall};

#[derive(Debug, Clone, Default)]
pub(crate) struct TargetReusePaths {
    pub(crate) explicit: Vec<PathBuf>,
    pub(crate) peers: Vec<PathBuf>,
}

impl TargetReusePaths {
    pub(crate) fn all(&self) -> Vec<PathBuf> {
        let mut seen = HashSet::with_capacity(self.explicit.len() + self.peers.len());
        self.explicit
            .iter()
            .chain(&self.peers)
            .filter(|path| seen.insert(path_key(path)))
            .cloned()
            .collect()
    }
}

pub(crate) async fn inspect_unique_installations(paths: &[PathBuf]) -> Result<Vec<LocalInstall>> {
    inspect_unique_paths(paths, "target").await
}

pub(crate) async fn inspect_unique_reuse_sources(paths: &[PathBuf]) -> Result<Vec<LocalInstall>> {
    let sources = inspect_unique_paths(paths, "reuse source").await?;
    for source in &sources {
        source.require_known_game().with_context(|| {
            format!(
                "Could not determine the game for reuse source {}",
                source.install_path.display()
            )
        })?;
    }
    Ok(sources)
}

pub(crate) fn validate_reuse_source_games(
    sources: &[LocalInstall],
    target_games: &[GameId],
) -> Result<()> {
    for source in sources {
        let source_game = source.require_known_game()?;
        if !target_games.iter().any(|game| game == &source_game) {
            anyhow::bail!(
                "Reuse source {} is for {}, but no selected target uses that game",
                source.install_path.display(),
                source_game
            );
        }
    }
    Ok(())
}

async fn inspect_unique_paths(paths: &[PathBuf], role: &str) -> Result<Vec<LocalInstall>> {
    if paths.is_empty() {
        if role == "target" {
            anyhow::bail!("At least one --path is required");
        }
        return Ok(Vec::new());
    }

    let mut installs = Vec::with_capacity(paths.len());
    let mut seen = HashSet::with_capacity(paths.len());
    for path in paths {
        let install = detect_local_install(path)
            .await
            .with_context(|| format!("Failed to inspect {role} {}", path.display()))?;
        let key = path_key(&install.install_path);
        if !seen.insert(key) {
            anyhow::bail!(
                "The same {role} was selected more than once: {}",
                install.install_path.display()
            );
        }
        installs.push(install);
    }
    Ok(installs)
}

pub(crate) fn reuse_paths_for_target(
    explicit_sources: &[LocalInstall],
    targets: &[LocalInstall],
    target_games: &[GameId],
    target_index: usize,
) -> TargetReusePaths {
    debug_assert_eq!(targets.len(), target_games.len());
    let target_key = path_key(&targets[target_index].install_path);
    let target_game = &target_games[target_index];
    let mut seen = HashSet::new();
    let mut explicit = Vec::new();
    let mut peers = Vec::new();

    for source in explicit_sources {
        if source.game_id.as_ref() != Some(target_game) {
            continue;
        }
        let key = path_key(&source.install_path);
        if key != target_key && seen.insert(key) {
            explicit.push(source.install_path.clone());
        }
    }

    for (index, source) in targets.iter().enumerate() {
        if index == target_index || target_games.get(index) != Some(target_game) {
            continue;
        }
        let key = path_key(&source.install_path);
        if key != target_key && seen.insert(key) {
            peers.push(source.install_path.clone());
        }
    }

    TargetReusePaths { explicit, peers }
}

fn path_key(path: &Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use griffr_common::runtime::ParsedConfigIni;
    use std::collections::BTreeMap;

    fn local(path: &str, game: GameId) -> LocalInstall {
        LocalInstall {
            install_path: PathBuf::from(path),
            config_ini: ParsedConfigIni {
                path: PathBuf::from(path).join("config.ini"),
                raw: String::new(),
                fields: BTreeMap::new(),
            },
            game_id: Some(game),
            region_id: None,
            channel_id: None,
        }
    }

    #[test]
    fn target_sources_keep_explicit_provenance_and_same_game_peers() {
        let targets = vec![
            local("A", GameId::ENDFIELD),
            local("B", GameId::ENDFIELD),
            local("C", GameId::ARKNIGHTS),
        ];
        let explicit = vec![
            local("D", GameId::ENDFIELD),
            local("B", GameId::ENDFIELD),
            local("E", GameId::ARKNIGHTS),
        ];

        let paths = reuse_paths_for_target(
            &explicit,
            &targets,
            &[GameId::ENDFIELD, GameId::ENDFIELD, GameId::ARKNIGHTS],
            0,
        );

        assert_eq!(paths.explicit, vec![PathBuf::from("D"), PathBuf::from("B")]);
        assert!(paths.peers.is_empty());
        assert_eq!(paths.all(), vec![PathBuf::from("D"), PathBuf::from("B")]);
    }

    #[test]
    fn peer_targets_become_sources_without_explicit_duplicates() {
        let targets = vec![local("A", GameId::ENDFIELD), local("B", GameId::ENDFIELD)];
        let explicit = vec![local("D", GameId::ENDFIELD)];

        let paths = reuse_paths_for_target(
            &explicit,
            &targets,
            &[GameId::ENDFIELD, GameId::ENDFIELD],
            0,
        );

        assert_eq!(paths.explicit, vec![PathBuf::from("D")]);
        assert_eq!(paths.peers, vec![PathBuf::from("B")]);
        assert_eq!(paths.all(), vec![PathBuf::from("D"), PathBuf::from("B")]);
    }

    #[test]
    fn effective_target_game_controls_peer_selection() {
        let targets = vec![local("A", GameId::ENDFIELD), local("B", GameId::ARKNIGHTS)];

        let paths = reuse_paths_for_target(&[], &targets, &[GameId::ENDFIELD, GameId::ENDFIELD], 0);

        assert_eq!(paths.peers, vec![PathBuf::from("B")]);
    }

    #[test]
    fn unmatched_explicit_source_game_is_rejected() {
        let sources = vec![local("C", GameId::ARKNIGHTS)];
        let error = validate_reuse_source_games(&sources, &[GameId::ENDFIELD]).unwrap_err();

        assert!(error.to_string().contains("no selected target"));
    }
}

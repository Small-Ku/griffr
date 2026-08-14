use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures_util::{stream, stream::FuturesUnordered, StreamExt, TryStreamExt};
use griffr_core::GameId;
use griffr_runtime::{detect_local_install, LocalInstall};

const PATH_INSPECTION_CONCURRENCY: usize = 8;

#[derive(Debug, Clone)]
pub(crate) struct BatchFailure {
    pub(crate) path: PathBuf,
    pub(crate) error: String,
}

pub(crate) fn validate_batch_options(batch: crate::BatchArgs) -> Result<()> {
    if !(1..=16).contains(&batch.jobs) {
        anyhow::bail!("--jobs must be between 1 and 16");
    }
    if batch.jobs > 1 && batch.fail_fast {
        anyhow::bail!("--fail-fast requires --jobs 1 so in-flight mutations are never abandoned");
    }
    Ok(())
}

fn volume_predecessors<T, K>(items: &[T], volume_keys: &K) -> Vec<Vec<usize>>
where
    K: Fn(&T) -> &[String],
{
    let mut last_user = HashMap::<&str, usize>::new();
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut dependencies = HashSet::new();
            let mut item_volumes = HashSet::new();
            for volume in volume_keys(item) {
                if !item_volumes.insert(volume) {
                    continue;
                }
                if let Some(previous) = last_user.insert(volume.as_str(), index) {
                    dependencies.insert(previous);
                }
            }
            let mut dependencies = dependencies.into_iter().collect::<Vec<_>>();
            dependencies.sort_unstable();
            dependencies
        })
        .collect()
}

pub(crate) fn volume_parallelism_bound<T, K>(
    items: &[T],
    max_parallel: usize,
    volume_keys: K,
) -> usize
where
    K: Fn(&T) -> &[String],
{
    let mut volumes = HashSet::new();
    let mut without_volume = 0usize;
    for item in items {
        let keys = volume_keys(item);
        if keys.is_empty() {
            without_volume = without_volume.saturating_add(1);
        } else {
            volumes.extend(keys.iter().map(String::as_str));
        }
    }
    max_parallel
        .min(items.len())
        .min(volumes.len().saturating_add(without_volume))
        .max(1)
}

pub(crate) async fn run_volume_dependency_graph<T, R, K, F, Fut>(
    items: Vec<T>,
    max_parallel: usize,
    volume_keys: K,
    mut run: F,
) -> Vec<R>
where
    K: Fn(&T) -> &[String],
    F: FnMut(T) -> Fut,
    Fut: Future<Output = R>,
{
    assert!(max_parallel > 0);
    if items.is_empty() {
        return Vec::new();
    }

    let predecessors = volume_predecessors(&items, &volume_keys);
    let mut successors = vec![Vec::<usize>::new(); items.len()];
    let mut remaining_dependencies = predecessors.iter().map(Vec::len).collect::<Vec<_>>();
    for (index, dependencies) in predecessors.into_iter().enumerate() {
        for dependency in dependencies {
            successors[dependency].push(index);
        }
    }

    let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
    let mut ready = remaining_dependencies
        .iter()
        .enumerate()
        .filter_map(|(index, &remaining)| (remaining == 0).then_some(index))
        .collect::<VecDeque<_>>();
    let mut running = FuturesUnordered::new();
    let mut results = Vec::with_capacity(items.len());

    while results.len() < items.len() {
        while running.len() < max_parallel {
            let Some(index) = ready.pop_front() else {
                break;
            };
            let item = items[index]
                .take()
                .expect("ready batch target is started only once");
            let future = run(item);
            running.push(async move { (index, future.await) });
        }

        let (finished, result) = running
            .next()
            .await
            .expect("volume dependency graph is acyclic and has ready work");
        results.push(result);
        for &successor in &successors[finished] {
            let remaining = &mut remaining_dependencies[successor];
            debug_assert!(*remaining > 0);
            *remaining -= 1;
            if *remaining == 0 {
                ready.push_back(successor);
            }
        }
    }

    results
}

pub(crate) fn print_batch_summary(operation: &str, total: usize, failures: &[BatchFailure]) {
    if total <= 1 {
        return;
    }
    let succeeded = total.saturating_sub(failures.len());
    crate::ui::print_info(format!(
        "{operation} batch: {succeeded} succeeded, {} failed, {total} total",
        failures.len()
    ));
    for failure in failures {
        crate::ui::print_warning(format!(
            "{} failed for {}: {}",
            operation,
            failure.path.display(),
            failure.error
        ));
    }
}

pub(crate) fn batch_error(operation: &str, failures: &[BatchFailure]) -> anyhow::Error {
    let details = failures
        .iter()
        .map(|failure| format!("{}: {}", failure.path.display(), failure.error))
        .collect::<Vec<_>>()
        .join("; ");
    anyhow::anyhow!(
        "{operation} failed for {} target(s): {details}",
        failures.len()
    )
}

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

    let installs = stream::iter(paths.iter())
        .map(|path| async move {
            detect_local_install(path)
                .await
                .with_context(|| format!("Failed to inspect {role} {}", path.display()))
        })
        .buffered(PATH_INSPECTION_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;

    let mut seen = HashSet::with_capacity(installs.len());
    for install in &installs {
        let key = path_key(&install.install_path);
        if !seen.insert(key) {
            anyhow::bail!(
                "The same {role} was selected more than once: {}",
                install.install_path.display()
            );
        }
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
    use griffr_runtime::{LocalInstallMetadata, ParsedConfigIni};
    use std::collections::BTreeMap;

    fn local(path: &str, game: GameId) -> LocalInstall {
        LocalInstall {
            install_path: PathBuf::from(path),
            metadata: LocalInstallMetadata::Hypergryph(ParsedConfigIni {
                path: PathBuf::from(path).join("config.ini"),
                raw: String::new(),
                fields: BTreeMap::new(),
            }),
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
    fn volume_parallelism_does_not_split_a_serial_volume_chain() {
        #[derive(Debug)]
        struct Item {
            keys: Vec<String>,
        }
        let same_volume = vec![
            Item {
                keys: vec!["a".into()],
            },
            Item {
                keys: vec!["a".into()],
            },
            Item {
                keys: vec!["a".into()],
            },
        ];
        let disjoint = vec![
            Item {
                keys: vec!["a".into()],
            },
            Item {
                keys: vec!["b".into()],
            },
            Item {
                keys: vec!["c".into()],
            },
        ];

        assert_eq!(
            volume_parallelism_bound(&same_volume, 16, |item| item.keys.as_slice()),
            1
        );
        assert_eq!(
            volume_parallelism_bound(&disjoint, 16, |item| item.keys.as_slice()),
            3
        );
    }

    #[compio::test]
    async fn volume_dependency_graph_releases_successors_without_a_global_barrier() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        #[derive(Debug)]
        struct Item {
            id: u8,
            keys: Vec<String>,
            delay: Duration,
        }

        let slow_a_finished = Arc::new(AtomicBool::new(false));
        let second_b_finished_before_a = Arc::new(AtomicBool::new(false));
        let observed_a = slow_a_finished.clone();
        let observed_b = second_b_finished_before_a.clone();
        let items = vec![
            Item {
                id: 1,
                keys: vec!["a".into()],
                delay: Duration::from_millis(80),
            },
            Item {
                id: 2,
                keys: vec!["b".into()],
                delay: Duration::from_millis(5),
            },
            Item {
                id: 3,
                keys: vec!["b".into()],
                delay: Duration::from_millis(5),
            },
        ];

        let results = run_volume_dependency_graph(
            items,
            2,
            |item| item.keys.as_slice(),
            move |item| {
                let slow_a_finished = observed_a.clone();
                let second_b_finished_before_a = observed_b.clone();
                async move {
                    compio::time::sleep(item.delay).await;
                    match item.id {
                        1 => slow_a_finished.store(true, Ordering::SeqCst),
                        3 => second_b_finished_before_a
                            .store(!slow_a_finished.load(Ordering::SeqCst), Ordering::SeqCst),
                        _ => {}
                    }
                    item.id
                }
            },
        )
        .await;

        assert_eq!(results.len(), 3);
        assert!(second_b_finished_before_a.load(Ordering::SeqCst));
    }

    #[test]
    fn volume_dependencies_only_link_conflicting_targets() {
        #[derive(Debug)]
        struct Item {
            keys: Vec<String>,
        }
        let items = vec![
            Item {
                keys: vec!["a".into()],
            },
            Item {
                keys: vec!["a".into()],
            },
            Item {
                keys: vec!["b".into()],
            },
            Item {
                keys: vec!["a".into(), "b".into()],
            },
            Item {
                keys: vec!["c".into(), "c".into()],
            },
        ];

        let dependencies = volume_predecessors(&items, &|item| item.keys.as_slice());

        assert_eq!(
            dependencies,
            vec![vec![], vec![0], vec![], vec![1, 2], vec![]]
        );
    }

    #[test]
    fn unmatched_explicit_source_game_is_rejected() {
        let sources = vec![local("C", GameId::ARKNIGHTS)];
        let error = validate_reuse_source_games(&sources, &[GameId::ENDFIELD]).unwrap_err();

        assert!(error.to_string().contains("no selected target"));
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::error::{Error, Result};

use super::{PatchPlan, PlannedPatchSource};

/// Returns topological waves for destructive patch application.
///
/// An HDiff consumer must run before any planned writer that replaces its base.
/// Grouping ready entries into waves gives both the runner and archive check space
/// simulator one authoritative dependency ordering.
pub(crate) fn entry_wave_indices(plan: &PatchPlan) -> Result<Vec<Vec<usize>>> {
    let mut destination_writers = BTreeMap::<PathBuf, usize>::new();
    for (index, entry) in plan.entries.iter().enumerate() {
        let logical_destination = plan
            .install_root
            .join(&plan.vfs_base_path)
            .join(&entry.name);
        let aliases = [entry.destination.clone(), logical_destination]
            .into_iter()
            .collect::<BTreeSet<_>>();
        for destination in aliases {
            if destination_writers
                .insert(destination.clone(), index)
                .is_some()
            {
                return Err(Error::Vfs(format!(
                    "Patch plan has multiple writers for {}",
                    destination.display()
                )));
            }
        }
    }

    let mut outgoing = vec![BTreeSet::<usize>::new(); plan.entries.len()];
    let mut indegree = vec![0usize; plan.entries.len()];
    for (consumer_index, entry) in plan.entries.iter().enumerate() {
        let PlannedPatchSource::Hdiff { base, .. } = &entry.source else {
            continue;
        };
        let Some(&writer_index) = destination_writers.get(base) else {
            continue;
        };
        if writer_index == consumer_index {
            continue;
        }
        if outgoing[consumer_index].insert(writer_index) {
            indegree[writer_index] = indegree[writer_index].saturating_add(1);
        }
    }

    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut waves = Vec::new();
    let mut completed = 0usize;
    while !ready.is_empty() {
        let wave = ready.iter().copied().collect::<Vec<_>>();
        ready.clear();
        completed = completed.saturating_add(wave.len());
        for index in &wave {
            for dependent in outgoing[*index].iter().copied() {
                indegree[dependent] = indegree[dependent].saturating_sub(1);
                if indegree[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
        }
        waves.push(wave);
    }
    if completed != plan.entries.len() {
        let blocked = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| {
                (*degree > 0).then_some(plan.entries[index].name.as_str())
            })
            .collect::<Vec<_>>();
        return Err(Error::Vfs(format!(
            "Patch dependency graph contains a destructive overwrite cycle: {}",
            blocked.join(", ")
        )));
    }
    Ok(waves)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::runtime::patch_transaction::{PlannedPatchEntry, PlannedPatchSource};

    fn plan(entries: Vec<PlannedPatchEntry>) -> PatchPlan {
        PatchPlan {
            schema_version: PatchPlan::SCHEMA_VERSION,
            install_root: PathBuf::from("/install"),
            stage_root: PathBuf::from("/stage"),
            vfs_base_path: PathBuf::from("vfs"),
            vfs_destination: PathBuf::from("/install/vfs"),
            work_dir: None,
            entries,
            delete_paths: Vec::new(),
            deferred_paths: Vec::new(),
        }
    }

    fn local(name: &str) -> PlannedPatchEntry {
        PlannedPatchEntry {
            name: name.to_string(),
            destination: PathBuf::from("/install/vfs").join(name),
            expected_md5: "md5".to_string(),
            expected_size: 1,
            source: PlannedPatchSource::Local {
                payload: PathBuf::from(".griffr-patch-stage/files").join(name),
            },
        }
    }

    #[test]
    fn consumer_precedes_writer_of_its_base() {
        let mut consumer = local("consumer");
        consumer.source = PlannedPatchSource::Hdiff {
            base: PathBuf::from("/install/vfs/base"),
            payload: PathBuf::from(".griffr-patch-stage/diffs/consumer"),
            base_md5: "base".to_string(),
            base_size: 1,
        };
        let waves = entry_wave_indices(&plan(vec![local("base"), consumer])).unwrap();
        assert_eq!(waves, vec![vec![1], vec![0]]);
    }
}

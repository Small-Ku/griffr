use super::super::super::*;
use super::fixture::{entry_names, RawSplitFixture};
use super::SAMPLE_SEED;
use crate::error::{Error, Result};
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn deterministic_sample_indices(
    inspection: &ArchiveInspection,
    layout: &MultiVolumeLayout,
    requested_count: usize,
) -> Vec<usize> {
    let names = entry_names(inspection);
    let candidates = names
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            name.as_ref()
                .is_some_and(|name| !name.ends_with('/'))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut selected = BTreeSet::new();
    selected.insert(candidates[0]);
    selected.insert(candidates[candidates.len() / 2]);
    selected.insert(*candidates.last().expect("non-empty candidates"));

    if let Some(index) = candidates
        .iter()
        .copied()
        .filter(|index| inspection.entry_sizes[*index] > 0)
        .min_by_key(|index| inspection.entry_sizes[*index])
    {
        selected.insert(index);
    }
    if let Some(index) = candidates
        .iter()
        .copied()
        .max_by_key(|index| inspection.entry_sizes[*index])
    {
        selected.insert(index);
    }
    selected.extend(inspection.control_indices.iter().copied());

    for volume_index in 0..layout.volume_count() {
        if let Some(index) = candidates
            .iter()
            .copied()
            .filter(|index| {
                inspection.entry_sources[*index]
                    .volume_indices
                    .contains(&volume_index)
            })
            .min_by_key(|index| inspection.entry_sizes[*index])
        {
            selected.insert(index);
        }
    }

    for boundary in layout
        .layouts
        .iter()
        .take(layout.volume_count().saturating_sub(1))
    {
        if let Some(index) = candidates.iter().copied().min_by_key(|index| {
            let source = &inspection.entry_sources[*index].range;
            source
                .start
                .abs_diff(boundary.end)
                .min(source.end.abs_diff(boundary.end))
        }) {
            selected.insert(index);
        }
    }

    let target = requested_count.max(selected.len()).min(candidates.len());
    let mut state = SAMPLE_SEED;
    while selected.len() < target {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        selected.insert(candidates[(state as usize) % candidates.len()]);
    }
    selected.into_iter().collect()
}

pub(super) fn bounded_samples(
    inspection: &ArchiveInspection,
    layout: &MultiVolumeLayout,
    requested_count: usize,
    byte_budget: u64,
) -> Vec<usize> {
    let mut candidates = deterministic_sample_indices(inspection, layout, requested_count);
    candidates.sort_by_key(|index| {
        (
            !inspection.control_indices.contains(index),
            inspection.entry_sizes[*index],
            *index,
        )
    });

    let mut selected = Vec::new();
    let mut bytes = 0u64;
    for index in candidates {
        let size = inspection.entry_sizes[index];
        if selected.len() >= requested_count && !inspection.control_indices.contains(&index) {
            continue;
        }
        if bytes.saturating_add(size) > byte_budget
            && !selected.is_empty()
            && !inspection.control_indices.contains(&index)
        {
            continue;
        }
        selected.push(index);
        bytes = bytes.saturating_add(size);
    }
    if selected.is_empty() {
        if let Some(index) = (0..inspection.entry_sizes.len())
            .filter(|index| {
                inspection
                    .archive
                    .name_for_index(*index)
                    .is_some_and(|name| !name.ends_with('/'))
            })
            .min_by_key(|index| inspection.entry_sizes[*index])
        {
            selected.push(index);
        }
    }
    selected
}

fn shadow_layout(
    original: &MultiVolumeLayout,
    present: &BTreeSet<usize>,
) -> Result<(tempfile::TempDir, MultiVolumeLayout)> {
    let parent = original
        .path(0)
        .and_then(Path::parent)
        .ok_or_else(|| Error::Extraction("archive fixture has no parent directory".to_string()))?;
    let temp = tempfile::Builder::new()
        .prefix(".griffr-archive-probe-")
        .tempdir_in(parent)?;
    let mut expected = Vec::with_capacity(original.volume_count());
    for (index, volume) in original.layouts.iter().enumerate() {
        let path = temp.path().join(format!("probe.zip.{:03}", index + 1));
        if present.contains(&index) {
            std::fs::hard_link(&volume.path, &path).map_err(|source| {
                Error::Extraction(format!(
                    "failed to hard-link fixture volume {} for isolated probing: {source}",
                    volume.path.display()
                ))
            })?;
        }
        expected.push((path, volume.end - volume.start));
    }
    Ok((temp, MultiVolumeLayout::from_expected(expected)?))
}

fn isolated_inspection(
    raw_fixture: &RawSplitFixture,
    required: &BTreeSet<usize>,
) -> Result<(tempfile::TempDir, MultiVolumeExtractor, ArchiveInspection)> {
    let central = raw_fixture
        .layout
        .volume_indices_for_range(raw_fixture.directory.central_directory.clone())
        .into_iter()
        .chain(
            raw_fixture
                .layout
                .volume_indices_for_range(raw_fixture.directory.end_records.clone()),
        )
        .chain(
            raw_fixture
                .layout
                .volume_indices_for_range(raw_fixture.layout.tail_probe_range()),
        )
        .collect::<BTreeSet<_>>();
    let present = required.union(&central).copied().collect::<BTreeSet<_>>();
    let (temp, layout) = shadow_layout(&raw_fixture.layout, &present)?;
    let extractor = MultiVolumeExtractor::from_layout(layout.clone());
    let mut inspection = extractor.inspect_archive_index(&raw_fixture.directory)?;

    inspection.archive = inspection.archive.clone();
    for index in central.difference(required) {
        if let Some(path) = layout.path(*index) {
            std::fs::remove_file(path)?;
        }
    }
    Ok((temp, extractor, inspection))
}

pub(super) fn validate_raw_sample(
    raw_fixture: &RawSplitFixture,
    index: usize,
    check_missing_start: bool,
) -> Result<()> {
    let required = raw_fixture.inspection.entry_sources[index]
        .volume_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if required.is_empty() {
        return Err(Error::Extraction(format!(
            "entry {index} has no declared source volumes"
        )));
    }

    let (temp, extractor, inspection) = isolated_inspection(raw_fixture, &required)?;
    let output = temp.path().join("output");
    std::fs::create_dir(&output)?;
    extractor.extract_entries_with_progress(
        &output,
        None,
        &inspection,
        &[index],
        &std::collections::BTreeMap::new(),
        64 * 1024,
        |_| {},
    )?;
    let name = inspection
        .archive
        .name_for_index(index)
        .ok_or_else(|| Error::Extraction(format!("entry {index} has no name")))?;
    let extracted = output.join(safe_relative_archive_path(name)?);
    let actual_size = std::fs::metadata(&extracted)?.len();
    if actual_size != inspection.entry_sizes[index] {
        return Err(Error::Extraction(format!(
            "sampled entry {name} extracted {actual_size} bytes, expected {}",
            inspection.entry_sizes[index]
        )));
    }

    if check_missing_start {
        let missing = *required
            .first()
            .expect("non-empty required volume set has a first item");
        let (temp, extractor, inspection) = isolated_inspection(raw_fixture, &required)?;
        let missing_path = extractor
            .layout
            .path(missing)
            .ok_or_else(|| Error::Extraction("missing probe volume path".to_string()))?
            .to_path_buf();
        std::fs::remove_file(missing_path)?;
        let output = temp.path().join("missing-output");
        std::fs::create_dir(&output)?;
        if extractor
            .extract_entries_with_progress(
                &output,
                None,
                &inspection,
                &[index],
                &std::collections::BTreeMap::new(),
                64 * 1024,
                |_| {},
            )
            .is_ok()
        {
            return Err(Error::Extraction(format!(
                "entry {index} extracted without its local-header volume {missing}"
            )));
        }
    }
    Ok(())
}

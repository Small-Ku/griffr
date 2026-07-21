use super::super::super::*;
use super::sampling::deterministic_sample_indices;
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct ArchiveSample {
    pub(super) key: PathBuf,
    pub(super) volumes: Vec<(u64, PathBuf)>,
}

#[derive(Debug, Serialize)]
pub(super) struct StandaloneVolumeProbe {
    pub(super) sequence: u64,
    pub(super) path: String,
    pub(super) eocd_at_eof: bool,
    pub(super) valid_zip: bool,
    pub(super) entry_count: Option<usize>,
    pub(super) error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ArchiveFormatReport {
    pub(super) archive: String,
    pub(super) sequences: Vec<u64>,
    pub(super) volume_count: usize,
    pub(super) standalone_zip_volumes: Vec<usize>,
    pub(super) format_kind: String,
    pub(super) entry_count: Option<usize>,
    pub(super) central_directory_volumes: Vec<usize>,
    pub(super) cross_volume_entries: Option<usize>,
    pub(super) sampled_entries: Vec<String>,
    pub(super) sampled_volume_coverage: Vec<usize>,
    pub(super) probes: Vec<StandaloneVolumeProbe>,
    pub(super) raw_split_error: Option<String>,
}

pub(super) struct RawSplitSample {
    pub(super) layout: MultiVolumeLayout,
    pub(super) directory: ArchiveDirectory,
    pub(super) archive_index: ArchiveIndex,
}

pub(super) struct CheckedSample {
    pub(super) report: ArchiveFormatReport,
    pub(super) raw_split: Option<RawSplitSample>,
}

fn tail_has_eocd_at_eof(path: &Path) -> Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let tail_size = size.min(EOCD_MAX_SEARCH);
    file.seek(SeekFrom::Start(size - tail_size))?;
    let mut tail = vec![0u8; tail_size as usize];
    file.read_exact(&mut tail)?;

    for offset in (0..=tail.len().saturating_sub(EOCD_MIN_SIZE as usize)).rev() {
        if tail.get(offset..offset + 4) != Some(&[0x50, 0x4b, 0x05, 0x06]) {
            continue;
        }
        let comment = usize::from(read_u16(&tail, offset + 20)?);
        if offset + EOCD_MIN_SIZE as usize + comment == tail.len() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn probe_standalone_volume(sequence: u64, path: &Path) -> Result<StandaloneVolumeProbe> {
    let eocd_at_eof = tail_has_eocd_at_eof(path)?;
    if !eocd_at_eof {
        return Ok(StandaloneVolumeProbe {
            sequence,
            path: path.display().to_string(),
            eocd_at_eof,
            valid_zip: false,
            entry_count: None,
            error: None,
        });
    }

    match zip::ZipArchive::new(std::fs::File::open(path)?) {
        Ok(archive) => Ok(StandaloneVolumeProbe {
            sequence,
            path: path.display().to_string(),
            eocd_at_eof,
            valid_zip: true,
            entry_count: Some(archive.len()),
            error: None,
        }),
        Err(error) => Ok(StandaloneVolumeProbe {
            sequence,
            path: path.display().to_string(),
            eocd_at_eof,
            valid_zip: false,
            entry_count: None,
            error: Some(error.to_string()),
        }),
    }
}

pub(super) fn entry_names(archive_index: &ArchiveIndex) -> Vec<Option<String>> {
    (0..archive_index.archive.len())
        .map(|index| {
            archive_index
                .archive
                .name_for_index(index)
                .map(str::to_owned)
        })
        .collect()
}

pub(super) fn check_sample_format(
    sample: &ArchiveSample,
    sample_count: usize,
) -> Result<CheckedSample> {
    let probes = sample
        .volumes
        .iter()
        .map(|(sequence, path)| probe_standalone_volume(*sequence, path))
        .collect::<Result<Vec<_>>>()?;
    let standalone_zip_volumes = probes
        .iter()
        .enumerate()
        .filter_map(|(index, probe)| probe.valid_zip.then_some(index))
        .collect::<Vec<_>>();

    let layout = MultiVolumeLayout::from_expected(
        sample
            .volumes
            .iter()
            .map(|(_, path)| Ok((path.clone(), std::fs::metadata(path)?.len())))
            .collect::<Result<Vec<_>>>()?,
    )?;
    let extractor = MultiVolumeExtractor::from_layout(layout.clone());
    let raw_result = (|| -> Result<(ArchiveDirectory, ArchiveIndex)> {
        let directory = match extractor.discover_archive_directory()? {
            ArchiveDirectoryDiscovery::Ready(directory) => directory,
            ArchiveDirectoryDiscovery::NeedsRange(range) => {
                return Err(Error::Message {
                    context: "Extraction error: ",
                    detail: format!(
                        "full sample unexpectedly needs range {}..{}",
                        range.start, range.end
                    ),
                });
            }
        };
        let archive_index = extractor.read_archive_index(&directory)?;
        Ok((directory, archive_index))
    })();

    let (format_kind, raw_split_error) = if standalone_zip_volumes.len() > 1 {
        let detail = match &raw_result {
            Ok((_, archive_index)) => format!(
                concat!(
                    "concatenated parsing also succeeded but exposed only {} entries; ",
                    "multiple volumes are independently readable ZIP archives"
                ),
                archive_index.archive.len()
            ),
            Err(error) => error.to_string(),
        };
        ("independent_archives", Some(detail))
    } else {
        match &raw_result {
            Ok(_) => ("raw_split", None),
            Err(error) if error.to_string().contains("Spanned") => {
                ("spanned_zip", Some(error.to_string()))
            }
            Err(error) => ("unrecognized", Some(error.to_string())),
        }
    };

    let mut raw_split = None;
    let (entry_count, central_directory_volumes, cross_volume_entries, sampled_entries, coverage) =
        if format_kind == "raw_split" {
            let (directory, archive_index) =
                raw_result.expect("raw_split format_kind has raw result");
            let samples = deterministic_sample_indices(&archive_index, &layout, sample_count);
            let names = entry_names(&archive_index);
            let sampled_entries = samples
                .iter()
                .filter_map(|index| names[*index].clone())
                .collect::<Vec<_>>();
            let coverage = samples
                .iter()
                .flat_map(|index| {
                    archive_index.entry_sources[*index]
                        .volume_indices
                        .iter()
                        .copied()
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let central_directory_volumes = layout
                .volume_indices_for_range(directory.central_directory.clone())
                .into_iter()
                .chain(layout.volume_indices_for_range(directory.end_records.clone()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let cross_volume_entries = archive_index
                .entry_sources
                .iter()
                .filter(|source| source.volume_indices.len() > 1)
                .count();
            let entry_count = archive_index.archive.len();
            raw_split = Some(RawSplitSample {
                layout,
                directory,
                archive_index,
            });
            (
                Some(entry_count),
                central_directory_volumes,
                Some(cross_volume_entries),
                sampled_entries,
                coverage,
            )
        } else {
            (None, Vec::new(), None, Vec::new(), Vec::new())
        };

    Ok(CheckedSample {
        report: ArchiveFormatReport {
            archive: sample.key.display().to_string(),
            sequences: sample
                .volumes
                .iter()
                .map(|(sequence, _)| *sequence)
                .collect(),
            volume_count: sample.volumes.len(),
            standalone_zip_volumes,
            format_kind: format_kind.to_string(),
            entry_count,
            central_directory_volumes,
            cross_volume_entries,
            sampled_entries,
            sampled_volume_coverage: coverage,
            probes,
            raw_split_error,
        },
        raw_split,
    })
}

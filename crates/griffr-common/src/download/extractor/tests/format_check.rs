use super::super::*;
use crate::error::{Error, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::api::ApiClient;
use md5::{Digest, Md5};

mod fixture;
mod sampling;

use fixture::{check_fixture_format, ArchiveFixture};
use sampling::{bounded_samples, deterministic_sample_indices, validate_raw_sample};

const OFFICIAL_SAMPLE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const OFFICIAL_SAMPLE_COUNT: usize = 8;
const SAMPLE_SEED: u64 = 0x4752_4946_4652;

fn official_cache_root(version: &str, packs: &[crate::api::types::PackFile]) -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::Extraction("failed to resolve workspace root".to_string()))?;
    let version = version
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut identity = Md5::new();
    for pack in packs {
        identity.update(pack.md5.as_bytes());
        identity.update(pack.size().to_le_bytes());
    }
    let identity = crate::to_hex(&identity.finalize());
    Ok(workspace
        .join("target/griffr-test-fixtures/archive-range-sample")
        .join(format!("{version}-{}", &identity[..16])))
}

async fn ensure_remote_ranges(
    layout: &MultiVolumeLayout,
    ranges: impl IntoIterator<Item = std::ops::Range<u64>>,
) -> Result<u64> {
    let requests = layout.missing_range_requests(ranges)?;
    let mut downloaded = 0u64;
    for request in requests {
        downloaded = downloaded.saturating_add(
            fetch_archive_range_to_cache(&request, "Mozilla/5.0", 256 * 1024, |_| {}).await?,
        );
        layout.register_range(&request)?;
    }
    Ok(downloaded)
}

fn tail_has_eocd_at_eof_bytes(tail: &[u8]) -> Result<bool> {
    if tail.len() < EOCD_MIN_SIZE as usize {
        return Ok(false);
    }
    for offset in (0..=tail.len() - EOCD_MIN_SIZE as usize).rev() {
        if tail.get(offset..offset + 4) != Some(&[0x50, 0x4b, 0x05, 0x06]) {
            continue;
        }
        let comment = usize::from(read_u16(tail, offset + 20)?);
        if offset + EOCD_MIN_SIZE as usize + comment == tail.len() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn make_zip(path: &Path, entries: &[(&str, &[u8])]) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored);
    for (name, payload) in entries {
        writer.start_file(*name, options)?;
        writer.write_all(payload)?;
    }
    writer.finish()?;
    Ok(())
}

fn split_archive_at(path: &Path, offsets: &[usize]) -> Result<Vec<PathBuf>> {
    let bytes = std::fs::read(path)?;
    let mut boundaries = offsets
        .iter()
        .copied()
        .filter(|offset| *offset > 0 && *offset < bytes.len())
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.push(bytes.len());

    let mut start = 0usize;
    let mut volumes = Vec::with_capacity(boundaries.len());
    for (index, end) in boundaries.into_iter().enumerate() {
        let volume = path.with_extension(format!("zip.{:03}", index + 1));
        std::fs::write(&volume, &bytes[start..end])?;
        volumes.push(volume);
        start = end;
    }
    Ok(volumes)
}

#[test]
fn raw_split_is_not_misclassified_as_independent_archives() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("raw.zip");
    make_zip(
        &archive,
        &[
            ("first.bin", &vec![1u8; 48_000]),
            ("second.bin", &vec![2u8; 48_000]),
        ],
    )?;
    let volumes = super::split_archive(&archive, 24_000)?;
    let fixture = ArchiveFixture {
        key: temp.path().join("raw.zip"),
        volumes: volumes
            .into_iter()
            .enumerate()
            .map(|(index, path)| ((index + 1) as u64, path))
            .collect(),
    };
    let checked = check_fixture_format(&fixture, 8)?;
    assert_eq!(checked.report.format_kind, "raw_split");
    assert!(checked.raw_split.is_some());
    assert!(checked.report.standalone_zip_volumes.len() <= 1);
    Ok(())
}

#[test]
fn independent_archives_get_separate_format_results() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut volumes = Vec::new();
    for index in 0..3 {
        let path = temp
            .path()
            .join(format!("independent.zip.{:03}", index + 1));
        let name = format!("payload-{index}.txt");
        let payload = format!("standalone payload {index}");
        make_zip(&path, &[(name.as_str(), payload.as_bytes())])?;
        volumes.push(((index + 1) as u64, path));
    }
    let fixture = ArchiveFixture {
        key: temp.path().join("independent.zip"),
        volumes,
    };
    let checked = check_fixture_format(&fixture, 8)?;
    assert_eq!(checked.report.format_kind, "independent_archives");
    assert_eq!(checked.report.standalone_zip_volumes, vec![0, 1, 2]);
    assert!(checked.raw_split.is_none());
    Ok(())
}

#[test]
fn central_directory_may_cross_a_raw_split_boundary() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("central-crossing.zip");
    let entries = (0..24)
        .map(|index| (format!("entry-{index:02}.txt"), vec![index as u8; 64]))
        .collect::<Vec<_>>();
    let borrowed = entries
        .iter()
        .map(|(name, payload)| (name.as_str(), payload.as_slice()))
        .collect::<Vec<_>>();
    make_zip(&archive, &borrowed)?;

    let single = MultiVolumeExtractor::new(vec![archive.clone()])?;
    let directory = match single.discover_archive_directory()? {
        ArchiveDirectoryDiscovery::Ready(directory) => directory,
        ArchiveDirectoryDiscovery::NeedsRange(_) => {
            return Err(Error::Extraction(
                "single-file fixture unexpectedly needs another range".to_string(),
            ));
        }
    };
    let split = directory.central_directory.start
        + (directory.central_directory.end - directory.central_directory.start) / 2;
    let volumes = split_archive_at(&archive, &[split as usize])?;
    let extractor = MultiVolumeExtractor::new(volumes)?;
    let split_directory = match extractor.discover_archive_directory()? {
        ArchiveDirectoryDiscovery::Ready(directory) => directory,
        ArchiveDirectoryDiscovery::NeedsRange(_) => {
            return Err(Error::Extraction(
                "complete split fixture unexpectedly needs another range".to_string(),
            ));
        }
    };
    assert_eq!(
        extractor
            .layout
            .volume_indices_for_range(split_directory.central_directory.clone())
            .len(),
        2
    );
    let archive_index = extractor.read_archive_index(&split_directory)?;
    assert_eq!(archive_index.archive.len(), entries.len());
    Ok(())
}

#[test]
fn declared_entry_volumes_are_sufficient_for_isolated_extraction() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("isolated.zip");
    make_zip(
        &archive,
        &[
            ("first.bin", &vec![1u8; 80_000]),
            ("second.bin", &vec![2u8; 80_000]),
            ("third.bin", &vec![3u8; 80_000]),
        ],
    )?;
    let volumes = super::split_archive(&archive, 24_000)?;
    let fixture = ArchiveFixture {
        key: temp.path().join("isolated.zip"),
        volumes: volumes
            .into_iter()
            .enumerate()
            .map(|(index, path)| ((index + 1) as u64, path))
            .collect(),
    };
    let checked = check_fixture_format(&fixture, 8)?;
    let raw_fixture = checked
        .raw_split
        .ok_or_else(|| Error::Extraction("fixture was not raw split".to_string()))?;
    let samples = deterministic_sample_indices(&raw_fixture.archive_index, &raw_fixture.layout, 8);
    for (position, index) in samples.into_iter().enumerate() {
        validate_raw_sample(&raw_fixture, index, position < 2)?;
    }
    Ok(())
}

#[test]
#[ignore = "downloads a bounded sample from the current official full package"]
fn check_official_archive_sample() -> Result<()> {
    compio::runtime::Runtime::new()
        .map_err(|error| Error::Extraction(format!("Failed to create compio runtime: {error}")))?
        .block_on(async {
            let client = ApiClient::new()?;
            let target = crate::config::resolve_api_target(
                &crate::config::GameId::ENDFIELD,
                crate::config::RegionId::Sg,
                &crate::config::ChannelPair::from_api("6", None::<String>)
                    .map_err(|error| Error::Extraction(error.to_string()))?,
                &crate::config::ApiTargetOverrides::default(),
            )
            .map_err(|error| Error::Extraction(error.to_string()))?;
            let info = client.get_latest_game(&target, None).await?;
            let package = info.pkg.as_ref().ok_or_else(|| {
                Error::Extraction("official response has no full package".to_string())
            })?;
            if package.packs.len() < 2 {
                return Err(Error::Extraction(format!(
                    "official full package is no longer multi-volume ({} part(s))",
                    package.packs.len()
                )));
            }

            let cache_root = official_cache_root(&info.version, &package.packs)?;
            let range_cache = cache_root.join("ranges");
            let volume_paths = cache_root.join("volumes");
            std::fs::create_dir_all(&volume_paths)?;
            let layout = MultiVolumeLayout::from_remote(
                package
                    .packs
                    .iter()
                    .map(|pack| {
                        let filename = pack.filename().ok_or_else(|| {
                            Error::Extraction("package URL has no filename".to_string())
                        })?;
                        Ok((
                            volume_paths.join(filename),
                            pack.url.clone(),
                            pack.size(),
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?,
                range_cache,
            )?;
            let extractor = MultiVolumeExtractor::from_layout(layout.clone());
            let mut downloaded = 0u64;

            // Check every official part with the same remote range
            // source used by production. Multiple per-volume EOCD records mean
            // the provider changed from one raw byte-split ZIP to independent
            // ZIP archives, which the production installer intentionally does
            // not support yet.
            let volume_tails = (0..layout.volume_count())
                .map(|index| {
                    layout.volume_tail_range(index).ok_or_else(|| {
                        Error::Extraction(format!("missing layout for volume {index}"))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            downloaded = downloaded.saturating_add(
                ensure_remote_ranges(&layout, volume_tails.iter().cloned()).await?,
            );
            let mut standalone_eocd_volumes = Vec::new();
            for (index, range) in volume_tails.iter().enumerate() {
                if tail_has_eocd_at_eof_bytes(&layout.read_range(range.clone())?)? {
                    standalone_eocd_volumes.push(index);
                }
            }
            if standalone_eocd_volumes.len() > 1 {
                return Err(Error::Extraction(format!(
                    "official package now exposes independent ZIP end records in volumes {standalone_eocd_volumes:?}"
                )));
            }
            if standalone_eocd_volumes.is_empty() {
                return Err(Error::Extraction(
                    "official package has no EOCD record at any volume boundary".to_string(),
                ));
            }

            let directory = loop {
                match extractor.discover_archive_directory()? {
                    ArchiveDirectoryDiscovery::Ready(directory) => break directory,
                    ArchiveDirectoryDiscovery::NeedsRange(range) => {
                        downloaded = downloaded
                            .saturating_add(ensure_remote_ranges(&layout, [range]).await?);
                    }
                }
            };
            downloaded = downloaded.saturating_add(
                ensure_remote_ranges(
                    &layout,
                    [
                        directory.central_directory.clone(),
                        directory.end_records.clone(),
                    ],
                )
                .await?,
            );
            let archive_index = extractor.read_archive_index(&directory)?;

            let expected_entries = client
                .fetch_game_files(&package.file_path, package.game_files_md5.as_deref())
                .await?;
            let expected = expected_entries
                .into_iter()
                .map(|entry| {
                    (
                        entry.path.replace('\\', "/").to_ascii_lowercase(),
                        entry,
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let archive_paths = archive_index
                .entries
                .keys()
                .map(|path| path.to_ascii_lowercase())
                .collect::<std::collections::BTreeSet<_>>();
            let matching = expected
                .keys()
                .filter(|path| archive_paths.contains(*path))
                .count();
            let minimum_matching = expected.len().saturating_mul(4) / 5;
            if matching < minimum_matching {
                return Err(Error::Extraction(format!(
                    "official archive index exposes only {matching}/{} manifest files; archive family or path semantics may have changed",
                    expected.len()
                )));
            }

            let mut samples = bounded_samples(
                &archive_index,
                &layout,
                OFFICIAL_SAMPLE_COUNT * 4,
                OFFICIAL_SAMPLE_MAX_BYTES,
            )
            .into_iter()
            .filter(|index| {
                archive_index
                    .archive
                    .name_for_index(*index)
                    .and_then(|name| normalized_archive_name(name).ok())
                    .is_some_and(|name| expected.contains_key(&name.to_ascii_lowercase()))
            })
            .take(OFFICIAL_SAMPLE_COUNT)
            .collect::<Vec<_>>();
            if samples.is_empty() {
                return Err(Error::Extraction(
                    "official archive sampling found no manifest-backed entries".to_string(),
                ));
            }
            samples.sort_unstable();
            downloaded = downloaded.saturating_add(
                ensure_remote_ranges(
                    &layout,
                    MultiVolumeExtractor::source_ranges_for_indices(&archive_index, &samples),
                )
                .await?,
            );

            let output = tempfile::tempdir()?;
            extractor.extract_entries_with_progress(
                output.path(),
                None,
                &archive_index,
                &samples,
                &expected,
                256 * 1024,
                |_| {},
            )?;
            println!(
                "validated {} official archive entries from {} raw-split volumes using {} ranged bytes ({} manifest paths matched; EOCD volume {:?})",
                samples.len(),
                layout.volume_count(),
                downloaded,
                matching,
                standalone_eocd_volumes
            );
            Ok(())
        })
}

use super::super::*;
use super::helpers::{extract_to_with_progress, split_archive};
use crate::api::types::GameFileEntry;
use crate::error::{Error, Result};
use md5::{Digest, Md5};
use std::io::Write;

#[test]
fn test_multi_volume_extractor() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("test.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("hello.txt", zip::write::FileOptions::<()>::default())?;
    zip.write_all(b"Hello, World!")?;
    zip.finish()?;

    let volumes = split_archive(&zip_path, 5)?;
    let extractor = MultiVolumeExtractor::new(volumes)?;
    let archive_index = extractor.read_patch_payload(None)?;
    assert_eq!(archive_index.archive.len(), 1);
    let output_dir = temp_dir.path().join("output");
    std::fs::create_dir(&output_dir)?;
    extract_to_with_progress(
        &extractor,
        &output_dir,
        None,
        &archive_index,
        2,
        64,
        None::<fn(u64, u64)>,
    )?;
    assert_eq!(
        std::fs::read_to_string(output_dir.join("hello.txt"))?,
        "Hello, World!"
    );
    Ok(())
}

#[test]
fn directory_discovery_only_opens_tail_range() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("large.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("payload.bin", options)?;
    zip.write_all(&vec![7u8; 160_000])?;
    zip.finish()?;

    let volumes = split_archive(&zip_path, 32_000)?;
    let expected = volumes
        .iter()
        .map(|path| Ok((path.clone(), std::fs::metadata(path)?.len())))
        .collect::<Result<Vec<_>>>()?;
    let layout = MultiVolumeLayout::from_expected(expected)?;
    let first = volumes[0].clone();
    let first_bytes = std::fs::read(&first)?;
    std::fs::remove_file(&first)?;

    let extractor = MultiVolumeExtractor::from_layout(layout.clone());
    let directory = match extractor.discover_archive_directory()? {
        ArchiveDirectoryDiscovery::Ready(directory) => directory,
        ArchiveDirectoryDiscovery::NeedsRange(range) => {
            return Err(Error::Extraction(format!(
                "unexpected ZIP64 dependency {}..{}",
                range.start, range.end
            )))
        }
    };
    assert_eq!(directory.entry_count, 1);
    assert!(directory.central_directory.start > 32_000);

    let archive_index = extractor.read_archive_index(&directory)?;
    std::fs::write(first, first_bytes)?;
    let shards = MultiVolumeExtractor::extraction_shards(&archive_index, 4);
    assert_eq!(shards.len(), 1);
    assert_eq!(shards[0].volume_indices.first(), Some(&0));
    assert!(shards[0].volume_indices.len() > 1);
    Ok(())
}

#[test]
fn directory_discovery_reports_unavailable_tail_range() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("missing-tail.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("payload.bin", zip::write::FileOptions::<()>::default())?;
    zip.write_all(&vec![3u8; 96_000])?;
    zip.finish()?;

    let volumes = split_archive(&zip_path, 24_000)?;
    let expected = volumes
        .iter()
        .map(|path| Ok((path.clone(), std::fs::metadata(path)?.len())))
        .collect::<Result<Vec<_>>>()?;
    let layout = MultiVolumeLayout::from_expected(expected)?;
    let missing_index = volumes.len() - 1;
    std::fs::remove_file(&volumes[missing_index])?;

    let extractor = MultiVolumeExtractor::from_layout(layout.clone());
    let range = match extractor.discover_archive_directory()? {
        ArchiveDirectoryDiscovery::NeedsRange(range) => range,
        ArchiveDirectoryDiscovery::Ready(_) => {
            return Err(Error::Extraction(
                "directory discovery ignored a missing tail volume".into(),
            ));
        }
    };
    assert!(layout
        .volume_indices_for_range(range)
        .contains(&missing_index));
    Ok(())
}

#[test]
fn extraction_shard_does_not_open_unrelated_volumes() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("sharded.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored);
    for index in 0..4 {
        zip.start_file(format!("payload-{index}.bin"), options)?;
        zip.write_all(&vec![index as u8; 96_000])?;
    }
    zip.finish()?;

    let volumes = split_archive(&zip_path, 24_000)?;
    let extractor = MultiVolumeExtractor::new(volumes.clone())?;
    let archive_index = extractor.read_patch_payload(None)?;
    let shard = MultiVolumeExtractor::extraction_shards(&archive_index, 4)
        .into_iter()
        .find(|shard| (0..volumes.len()).any(|index| !shard.volume_indices.contains(&index)))
        .ok_or_else(|| Error::Extraction("test archive produced no range-local shard".into()))?;
    let missing_index = (0..volumes.len())
        .find(|index| !shard.volume_indices.contains(index))
        .expect("range-local shard has an unrelated volume");
    std::fs::remove_file(&volumes[missing_index])?;

    let output_dir = temp_dir.path().join("range-output");
    std::fs::create_dir(&output_dir)?;
    extractor.extract_entries_with_progress(
        &output_dir,
        None,
        &archive_index,
        &shard.entries,
        &std::collections::BTreeMap::new(),
        64 * 1024,
        |_| {},
    )?;

    let extracted_files = std::fs::read_dir(&output_dir)?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .count();
    assert_eq!(extracted_files, shard.entries.len());
    Ok(())
}

#[test]
fn extraction_shards_preserve_release_frontiers_when_budget_allows() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("frontiers.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored);
    for index in 0..8 {
        zip.start_file(format!("payload-{index}.bin"), options)?;
        zip.write_all(&vec![index as u8; 48_000])?;
    }
    zip.finish()?;

    let volumes = split_archive(&zip_path, 24_000)?;
    let extractor = MultiVolumeExtractor::new(volumes)?;
    let archive_index = extractor.read_patch_payload(None)?;
    let expected_frontiers = archive_index
        .entry_sources
        .iter()
        .map(|source| source.volume_indices.last().copied().unwrap_or(0))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(expected_frontiers.len() > 1);

    let shards =
        MultiVolumeExtractor::extraction_shards(&archive_index, archive_index.entry_sizes.len());
    let mut actual_frontiers = std::collections::BTreeSet::new();
    for shard in shards {
        let frontiers = shard
            .entries
            .iter()
            .map(|index| {
                archive_index.entry_sources[*index]
                    .volume_indices
                    .last()
                    .copied()
                    .unwrap_or(0)
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            frontiers.len(),
            1,
            "a shard crossed release frontiers despite sufficient shard budget"
        );
        actual_frontiers.extend(frontiers);
    }
    assert_eq!(actual_frontiers, expected_frontiers);
    Ok(())
}

#[test]
fn extraction_shards_bound_compressed_source_chunks() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("bounded-source.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored);
    for index in 0..6 {
        zip.start_file(format!("payload-{index}.bin"), options)?;
        zip.write_all(&vec![index as u8; 40_000])?;
    }
    zip.finish()?;

    let extractor = MultiVolumeExtractor::new(vec![zip_path])?;
    let archive_index = extractor.read_patch_payload(None)?;
    let source_limit = 70_000;
    let shards =
        MultiVolumeExtractor::extraction_shards_with_source_limit(&archive_index, 1, source_limit);
    assert!(
        shards.len() > 1,
        "one release frontier remained one large range barrier"
    );
    for shard in shards {
        let source_bytes = shard
            .entries
            .iter()
            .map(|index| {
                let range = &archive_index.entry_sources[*index].range;
                range.end - range.start
            })
            .sum::<u64>();
        assert!(
            source_bytes <= source_limit || shard.entries.len() == 1,
            "multi-entry shard exceeded its compressed source bound"
        );
    }
    Ok(())
}

#[test]
fn extraction_shards_never_merge_distinct_release_frontiers() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("frontier-budget.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored);
    for index in 0..8 {
        zip.start_file(format!("payload-{index}.bin"), options)?;
        zip.write_all(&vec![index as u8; 48_000])?;
    }
    zip.finish()?;

    let volumes = split_archive(&zip_path, 24_000)?;
    let extractor = MultiVolumeExtractor::new(volumes)?;
    let archive_index = extractor.read_patch_payload(None)?;
    let expected_frontiers = archive_index
        .entry_sources
        .iter()
        .map(|source| source.volume_indices.last().copied().unwrap_or(0))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(expected_frontiers.len() > 2);

    let shards = MultiVolumeExtractor::extraction_shards(&archive_index, 2);
    let actual_frontiers = shards
        .iter()
        .map(|shard| {
            shard
                .entries
                .iter()
                .map(|index| {
                    archive_index.entry_sources[*index]
                        .volume_indices
                        .last()
                        .copied()
                        .unwrap_or(0)
                })
                .collect::<std::collections::BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    assert!(actual_frontiers
        .iter()
        .all(|frontiers| frontiers.len() == 1));
    assert_eq!(
        actual_frontiers
            .into_iter()
            .flatten()
            .collect::<std::collections::BTreeSet<_>>(),
        expected_frontiers
    );
    Ok(())
}

#[test]
fn spanned_zip_metadata_is_rejected() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("spanned.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("payload.bin", zip::write::FileOptions::<()>::default())?;
    zip.write_all(b"payload")?;
    zip.finish()?;

    let mut bytes = std::fs::read(&zip_path)?;
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == [0x50, 0x4b, 0x05, 0x06])
        .expect("test ZIP has an EOCD record");
    bytes[eocd + 4..eocd + 6].copy_from_slice(&1u16.to_le_bytes());
    std::fs::write(&zip_path, bytes)?;

    let extractor = MultiVolumeExtractor::new(vec![zip_path])?;
    let error = extractor.discover_archive_directory().unwrap_err();
    assert!(error.to_string().contains("Spanned"));
    Ok(())
}

#[test]
fn extraction_checks_manifest_md5_and_removes_bad_output() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let zip_path = temp_dir.path().join("verified.zip");
    let payload = b"manifest verified payload";
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("Data/Payload.bin", zip::write::FileOptions::<()>::default())?;
    zip.write_all(payload)?;
    zip.finish()?;

    let extractor = MultiVolumeExtractor::new(vec![zip_path])?;
    let archive_index = extractor.read_patch_payload(None)?;
    let entries = (0..archive_index.archive.len()).collect::<Vec<_>>();
    let expected_md5 = crate::to_hex(&Md5::digest(payload));
    let expected = std::collections::BTreeMap::from([(
        "data/payload.bin".to_string(),
        GameFileEntry {
            path: "Data/Payload.bin".to_string(),
            md5: expected_md5,
            size: payload.len() as u64,
        },
    )]);

    let valid_output = temp_dir.path().join("valid-output");
    std::fs::create_dir_all(&valid_output)?;
    extractor.extract_entries_with_progress(
        &valid_output,
        None,
        &archive_index,
        &entries,
        &expected,
        64 * 1024,
        |_| {},
    )?;
    assert_eq!(
        std::fs::read(valid_output.join("Data/Payload.bin"))?,
        payload
    );

    let invalid_output = temp_dir.path().join("invalid-output");
    std::fs::create_dir_all(&invalid_output)?;
    let mut invalid_expected = expected;
    invalid_expected
        .get_mut("data/payload.bin")
        .expect("expected fixture entry")
        .md5 = "00000000000000000000000000000000".to_string();
    let error = extractor
        .extract_entries_with_progress(
            &invalid_output,
            None,
            &archive_index,
            &entries,
            &invalid_expected,
            64 * 1024,
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("failed target verification"));
    assert!(!invalid_output.join("Data/Payload.bin").exists());
    Ok(())
}

#[test]
fn range_cache_prunes_only_segments_without_pending_readers() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let cache = temp_dir.path().join("ranges");
    let layout = MultiVolumeLayout::from_remote(
        vec![(
            temp_dir.path().join("payload.zip.001"),
            "https://example.invalid/payload.zip.001".to_string(),
            1_000,
        )],
        cache.clone(),
    )?;
    let first = ArchiveRangeRequest {
        volume_index: 0,
        local_range: 0..100,
        global_range: 0..100,
        url: "https://example.invalid/payload.zip.001".to_string(),
        cache_path: cache.join("v0000-0-100.range"),
    };
    let second = ArchiveRangeRequest {
        volume_index: 0,
        local_range: 200..300,
        global_range: 200..300,
        url: "https://example.invalid/payload.zip.001".to_string(),
        cache_path: cache.join("v0000-200-300.range"),
    };
    std::fs::write(&first.cache_path, vec![1u8; 100])?;
    std::fs::write(&second.cache_path, vec![2u8; 100])?;
    layout.register_range(&first)?;
    layout.register_range(&second)?;

    layout.prune_range_cache(std::slice::from_ref(&(200..300)));
    assert!(!first.cache_path.exists());
    assert!(second.cache_path.exists());
    assert!(!layout.range_is_available(&(0..100)));
    assert!(layout.range_is_available(&(200..300)));

    layout.prune_range_cache(&[]);
    assert!(!second.cache_path.exists());
    assert!(!layout.range_is_available(&(200..300)));
    Ok(())
}

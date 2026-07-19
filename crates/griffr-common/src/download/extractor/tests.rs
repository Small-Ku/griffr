use super::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

enum ExtractShardEvent {
    Bytes(u64),
    Finished(std::result::Result<(), String>),
}

fn extract_to_with_progress(
    extractor: &MultiVolumeExtractor,
    target_dir: &Path,
    password: Option<&str>,
    inspection: &ArchiveInspection,
    max_shards: usize,
    progress_buffer_bytes: usize,
    mut progress_callback: Option<impl FnMut(u64, u64)>,
) -> Result<()> {
    if let Some(callback) = progress_callback.as_mut() {
        callback(0, inspection.total_uncompressed_bytes);
    }
    let shards = MultiVolumeExtractor::extraction_shards(inspection, max_shards);
    if shards.is_empty() {
        return Ok(());
    }

    let (progress_tx, progress_rx) = mpsc::channel();
    let mut errors = Vec::new();
    let mut extracted_bytes = 0u64;
    std::thread::scope(|scope| {
        let handles = shards
            .into_iter()
            .map(|shard| {
                let tx = progress_tx.clone();
                scope.spawn(move || {
                    let result = extractor
                        .extract_entries_with_progress(
                            target_dir,
                            password,
                            inspection,
                            &shard.entries,
                            progress_buffer_bytes,
                            |bytes| {
                                let _ = tx.send(ExtractShardEvent::Bytes(bytes));
                            },
                        )
                        .map_err(|error| error.to_string());
                    let _ = tx.send(ExtractShardEvent::Finished(result));
                })
            })
            .collect::<Vec<_>>();
        drop(progress_tx);

        for event in progress_rx {
            match event {
                ExtractShardEvent::Bytes(bytes) => {
                    extracted_bytes = extracted_bytes.saturating_add(bytes);
                    if let Some(callback) = progress_callback.as_mut() {
                        callback(extracted_bytes, inspection.total_uncompressed_bytes);
                    }
                }
                ExtractShardEvent::Finished(Err(error)) => errors.push(error),
                ExtractShardEvent::Finished(Ok(())) => {}
            }
        }
        for handle in handles {
            if handle.join().is_err() {
                errors.push("archive extraction shard panicked".to_string());
            }
        }
    });

    if !errors.is_empty() {
        return Err(Error::Extraction(errors.join("; ")));
    }
    if let Some(callback) = progress_callback.as_mut() {
        callback(
            inspection.total_uncompressed_bytes,
            inspection.total_uncompressed_bytes,
        );
    }
    Ok(())
}

fn split_archive(path: &Path, chunk_size: usize) -> Result<Vec<PathBuf>> {
    let data = std::fs::read(path)?;
    let mut volumes = Vec::new();
    for (index, chunk) in data.chunks(chunk_size).enumerate() {
        let volume_path = path.with_extension(format!("zip.{:03}", index + 1));
        std::fs::write(&volume_path, chunk)?;
        volumes.push(volume_path);
    }
    Ok(volumes)
}

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
    let inspection = extractor.inspect_patch_payload(None)?;
    assert_eq!(inspection.archive.len(), 1);
    let output_dir = temp_dir.path().join("output");
    std::fs::create_dir(&output_dir)?;
    extract_to_with_progress(
        &extractor,
        &output_dir,
        None,
        &inspection,
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

    // Index parsing must use only the central-directory/end-record ranges.
    // The first payload volume remains absent until after inspection.
    let inspection = extractor.inspect_archive_index(&directory)?;
    std::fs::write(first, first_bytes)?;
    let shards = MultiVolumeExtractor::extraction_shards(&inspection, 4);
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
    let zip_path = temp_dir.path().join("pipelined.zip");
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
    let inspection = extractor.inspect_patch_payload(None)?;
    let shard = MultiVolumeExtractor::extraction_shards(&inspection, 4)
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
        &inspection,
        &shard.entries,
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
    let inspection = extractor.inspect_patch_payload(None)?;
    let expected_frontiers = inspection
        .entry_sources
        .iter()
        .map(|source| source.volume_indices.last().copied().unwrap_or(0))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(expected_frontiers.len() > 1);

    let shards = MultiVolumeExtractor::extraction_shards(&inspection, inspection.entry_sizes.len());
    let mut actual_frontiers = std::collections::BTreeSet::new();
    for shard in shards {
        let frontiers = shard
            .entries
            .iter()
            .map(|index| {
                inspection.entry_sources[*index]
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

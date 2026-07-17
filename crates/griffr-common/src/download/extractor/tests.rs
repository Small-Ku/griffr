use super::*;
use std::io::Write;
use std::path::Path;
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
    let ranges = MultiVolumeExtractor::extraction_ranges(inspection, max_shards);
    if ranges.is_empty() {
        return Ok(());
    }

    let (progress_tx, progress_rx) = mpsc::channel();
    let mut errors = Vec::new();
    let mut extracted_bytes = 0u64;
    std::thread::scope(|scope| {
        let handles = ranges
            .into_iter()
            .map(|range| {
                let tx = progress_tx.clone();
                scope.spawn(move || {
                    let result = extractor
                        .extract_range_with_progress(
                            target_dir,
                            password,
                            inspection,
                            range,
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

#[test]
fn test_multi_volume_extractor() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let base_path = temp_dir.path();

    // 1. Create a zip archive and split it
    let zip_path = base_path.join("test.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("hello.txt", zip::write::FileOptions::<()>::default())?;
    zip.write_all(b"Hello, World!")?;
    zip.finish()?;

    let data = std::fs::read(&zip_path)?;
    let chunk_size = 5;
    let mut volumes = Vec::new();
    for (i, chunk) in data.chunks(chunk_size).enumerate() {
        let volume_path = base_path.join(format!("test.zip.{:03}", i + 1));
        std::fs::write(&volume_path, chunk)?;
        volumes.push(volume_path);
    }

    // 2. Extract
    let extractor = MultiVolumeExtractor::new(volumes)?;
    let inspection = extractor.inspect_patch_payload(None)?;
    let archive_clone = inspection.archive.clone();
    assert_eq!(archive_clone.len(), inspection.archive.len());
    assert_eq!(
        archive_clone.central_directory_start(),
        inspection.archive.central_directory_start()
    );
    let output_dir = base_path.join("output");
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

    // 3. Verify
    let output_file = output_dir.join("hello.txt");
    assert!(output_file.exists());
    let content = std::fs::read_to_string(output_file)?;
    assert_eq!(content, "Hello, World!");

    Ok(())
}

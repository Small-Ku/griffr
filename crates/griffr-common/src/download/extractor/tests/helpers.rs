use super::super::*;
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub(crate) enum ExtractShardEvent {
    Bytes(u64),
    Finished(std::result::Result<(), String>),
}

pub(crate) fn extract_to_with_progress(
    extractor: &MultiVolumeExtractor,
    target_dir: &Path,
    password: Option<&str>,
    archive_index: &ArchiveIndex,
    max_shards: usize,
    progress_buffer_bytes: usize,
    mut progress_callback: Option<impl FnMut(u64, u64)>,
) -> Result<()> {
    if let Some(callback) = progress_callback.as_mut() {
        callback(0, archive_index.total_uncompressed_bytes);
    }
    let shards = MultiVolumeExtractor::extraction_shards(archive_index, max_shards);
    if shards.is_empty() {
        return Ok(());
    }

    let (progress_tx, progress_rx) = mpsc::channel();
    let expected_files = std::collections::BTreeMap::new();
    let expected_files_ref = &expected_files;
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
                            archive_index,
                            &shard.entries,
                            expected_files_ref,
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
                        callback(extracted_bytes, archive_index.total_uncompressed_bytes);
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
        return Err(Error::Message {
            context: "Extraction error: ",
            detail: errors.join("; "),
        });
    }
    if let Some(callback) = progress_callback.as_mut() {
        callback(
            archive_index.total_uncompressed_bytes,
            archive_index.total_uncompressed_bytes,
        );
    }
    Ok(())
}

pub(crate) fn split_archive(path: &Path, chunk_size: usize) -> Result<Vec<PathBuf>> {
    let data = std::fs::read(path)?;
    let mut volumes = Vec::new();
    for (index, chunk) in data.chunks(chunk_size).enumerate() {
        let volume_path = path.with_extension(format!("zip.{:03}", index + 1));
        std::fs::write(&volume_path, chunk)?;
        volumes.push(volume_path);
    }
    Ok(volumes)
}

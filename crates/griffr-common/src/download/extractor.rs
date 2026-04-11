//! Multi-volume zip extraction

use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

/// Multi-volume stream that reads split zip files as a single stream
pub struct MultiVolumeStream {
    volumes: Vec<PathBuf>,
    current_volume: usize,
    current_file: Option<std::fs::File>,
    position: u64,
}

impl MultiVolumeStream {
    /// Create a new multi-volume stream from a list of volume paths
    pub fn new(volumes: Vec<PathBuf>) -> Result<Self> {
        if volumes.is_empty() {
            anyhow::bail!("No volumes provided");
        }

        // Verify all volumes exist
        for volume in &volumes {
            if !volume.exists() {
                anyhow::bail!("Volume not found: {}", volume.display());
            }
        }

        let mut stream = Self {
            volumes,
            current_volume: 0,
            current_file: None,
            position: 0,
        };

        // Open first volume
        stream.open_current_volume()?;

        Ok(stream)
    }

    /// Create from a base path and pattern
    /// For example: `Beyond_Release.zip.001`, `Beyond_Release.zip.002`, etc.
    pub fn from_pattern(base_path: &Path, pattern: &str) -> Result<Self> {
        let mut volumes = Vec::new();
        let mut index = 1;

        loop {
            let volume_path = base_path.join(format!("{}.{:03}", pattern, index));
            if volume_path.exists() {
                volumes.push(volume_path);
                index += 1;
            } else {
                break;
            }
        }

        if volumes.is_empty() {
            // Try with the pattern as a full filename stem
            let mut index = 1;
            loop {
                let volume_path = base_path.with_extension(format!("zip.{:03}", index));
                if volume_path.exists() {
                    volumes.push(volume_path);
                    index += 1;
                } else {
                    break;
                }
            }
        }

        Self::new(volumes)
    }

    /// Open the current volume file
    fn open_current_volume(&mut self) -> Result<()> {
        if self.current_volume >= self.volumes.len() {
            anyhow::bail!("No more volumes to open");
        }

        let path = &self.volumes[self.current_volume];
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open volume: {}", path.display()))?;

        self.current_file = Some(file);
        Ok(())
    }

    /// Move to the next volume
    fn next_volume(&mut self) -> Result<bool> {
        self.current_volume += 1;
        if self.current_volume < self.volumes.len() {
            self.open_current_volume()?;
            Ok(true)
        } else {
            self.current_file = None;
            Ok(false)
        }
    }

    /// Get the total size of all volumes
    pub fn total_size(&self) -> Result<u64> {
        let mut total = 0u64;
        for volume in &self.volumes {
            let metadata = std::fs::metadata(volume)?;
            total += metadata.len();
        }
        Ok(total)
    }
}

impl Read for MultiVolumeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match &mut self.current_file {
                Some(file) => {
                    let bytes_read = file.read(buf)?;
                    if bytes_read > 0 {
                        self.position += bytes_read as u64;
                        return Ok(bytes_read);
                    }

                    // End of current volume, try next
                    match self.next_volume() {
                        Ok(true) => continue,
                        Ok(false) => return Ok(0),
                        Err(e) => return Err(std::io::Error::other(e)),
                    }
                }
                None => return Ok(0),
            }
        }
    }
}

impl Seek for MultiVolumeStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        // For multi-volume streams, we need to calculate which volume and offset
        // This is a simplified implementation
        let target_position = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => self.position as i64 + offset,
            SeekFrom::End(offset) => {
                let total_size = self.total_size().map_err(std::io::Error::other)? as i64;
                total_size + offset
            }
        };

        let total_size = self.total_size().map_err(std::io::Error::other)?;

        if target_position < 0 || target_position > total_size as i64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Invalid seek position: {} (total size: {})",
                    target_position, total_size
                ),
            ));
        }

        let target = target_position as u64;

        // Special case for seeking to EOF
        if target == total_size {
            let last_idx = self.volumes.len() - 1;
            if self.current_volume != last_idx {
                self.current_volume = last_idx;
                self.open_current_volume().map_err(std::io::Error::other)?;
            }
            if let Some(file) = &mut self.current_file {
                file.seek(SeekFrom::End(0))?;
            }
            self.position = target;
            return Ok(target);
        }

        // Find the volume containing this position
        let mut cumulative_size = 0u64;
        for (i, volume) in self.volumes.iter().enumerate() {
            let volume_size = std::fs::metadata(volume)
                .map_err(std::io::Error::other)?
                .len();

            if target >= cumulative_size && target < cumulative_size + volume_size {
                // Target is in this volume
                if self.current_volume != i {
                    self.current_volume = i;
                    self.open_current_volume().map_err(std::io::Error::other)?;
                }

                let offset_in_volume = target - cumulative_size;
                if let Some(file) = &mut self.current_file {
                    file.seek(SeekFrom::Start(offset_in_volume))?;
                }

                self.position = target;
                return Ok(target);
            }

            cumulative_size += volume_size;
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Seek position beyond end of stream",
        ))
    }
}

/// Multi-volume zip extractor
pub struct MultiVolumeExtractor {
    volumes: Vec<PathBuf>,
}

impl MultiVolumeExtractor {
    /// Create a new extractor from a list of volume paths
    pub fn new(volumes: Vec<PathBuf>) -> Result<Self> {
        if volumes.is_empty() {
            anyhow::bail!("No volumes provided");
        }

        Ok(Self { volumes })
    }

    /// Create from a directory and base filename.
    /// Supports either split archives like `base_name.zip.001`, `base_name.zip.002`, etc.
    /// or a single archive named `base_name.zip`.
    pub fn from_directory(dir: &Path, base_name: &str) -> Result<Self> {
        let mut volumes = Vec::new();
        let mut index = 1;

        loop {
            let volume_path = dir.join(format!("{}.zip.{:03}", base_name, index));
            if volume_path.exists() {
                volumes.push(volume_path);
                index += 1;
            } else {
                break;
            }
        }

        if volumes.is_empty() {
            let single_archive = dir.join(format!("{}.zip", base_name));
            if single_archive.exists() {
                volumes.push(single_archive);
            }
        }

        if volumes.is_empty() {
            anyhow::bail!("No volumes found for {} in {}", base_name, dir.display());
        }

        Self::new(volumes)
    }

    /// Extract all volumes to the target directory
    pub fn extract_to(&self, target_dir: &Path) -> Result<()> {
        fn safe_join(target_dir: &Path, name: &str) -> Result<PathBuf> {
            let rel = Path::new(name);

            if rel.is_absolute() {
                anyhow::bail!("Zip entry has absolute path: {}", name);
            }

            // Prevent Zip Slip: reject any path containing '..', root dir, or Windows prefixes.
            for c in rel.components() {
                match c {
                    Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                        anyhow::bail!("Zip entry has unsafe path: {}", name);
                    }
                    _ => {}
                }
            }

            Ok(target_dir.join(rel))
        }

        let stream = MultiVolumeStream::new(self.volumes.clone())?;
        let mut archive =
            zip::ZipArchive::new(stream).context("Failed to open multi-volume zip archive")?;

        // Extract all files
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let file_path = safe_join(target_dir, file.name())?;

            if file.is_dir() {
                std::fs::create_dir_all(&file_path).with_context(|| {
                    format!("Failed to create directory: {}", file_path.display())
                })?;
                continue;
            }

            // Create parent directory if needed
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create parent directory: {}", parent.display())
                })?;
            }

            // Extract file (overwrite if exists)
            let mut output = std::fs::File::create(&file_path)
                .with_context(|| format!("Failed to create file: {}", file_path.display()))?;
            std::io::copy(&mut file, &mut output)
                .with_context(|| format!("Failed to extract file: {}", file.name()))?;
        }

        Ok(())
    }

    /// Get the list of files in the archive without extracting
    pub fn list_files(&self) -> Result<Vec<String>> {
        let stream = MultiVolumeStream::new(self.volumes.clone())?;
        let mut archive =
            zip::ZipArchive::new(stream).context("Failed to open multi-volume zip archive")?;

        let mut files = Vec::new();
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            files.push(file.name().to_string());
        }

        Ok(files)
    }

    /// Delete volumes after successful extraction
    pub fn cleanup(&self) -> Result<()> {
        for volume in &self.volumes {
            if let Err(e) = std::fs::remove_file(volume) {
                tracing::warn!("Failed to delete volume {}: {}", volume.display(), e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        let output_dir = base_path.join("output");
        std::fs::create_dir(&output_dir)?;
        extractor.extract_to(&output_dir)?;

        // 3. Verify
        let output_file = output_dir.join("hello.txt");
        assert!(output_file.exists());
        let content = std::fs::read_to_string(output_file)?;
        assert_eq!(content, "Hello, World!");

        Ok(())
    }
}

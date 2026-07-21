use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::PathBuf;

use super::volume::MultiVolumeLayout;
use crate::error::{Error, Result};

/// Seekable view over full-volume files and downloaded range segments.
#[derive(Debug)]
pub struct MultiVolumeStream {
    pub(super) layout: MultiVolumeLayout,
    current_path: Option<PathBuf>,
    current_file: Option<std::fs::File>,
    current_range: Range<u64>,
    position: u64,
}

impl MultiVolumeStream {
    pub(super) fn from_layout(layout: MultiVolumeLayout) -> Result<Self> {
        if layout.layouts.is_empty() {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: "No volumes provided".to_string(),
            });
        }
        Ok(Self {
            layout,
            current_path: None,
            current_file: None,
            current_range: 0..0,
            position: 0,
        })
    }

    fn select_segment(
        &mut self,
        volume_index: usize,
        local_offset: u64,
    ) -> std::io::Result<(u64, u64)> {
        let cached = {
            let ranges = self.layout.ranges.lock().unwrap();
            ranges.get(volume_index).and_then(|ranges| {
                ranges
                    .iter()
                    .filter(|range| {
                        range.range.start <= local_offset && range.range.end > local_offset
                    })
                    .min_by_key(|range| range.range.end - range.range.start)
                    .cloned()
            })
        }
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "archive byte range for volume {volume_index} at offset \
                     {local_offset} is not cached"
                ),
            )
        })?;
        if self.current_path.as_ref() != Some(&cached.path) || self.current_file.is_none() {
            self.current_file = Some(std::fs::File::open(&cached.path)?);
            self.current_path = Some(cached.path.clone());
            self.current_range = cached.range.clone();
        }
        let segment_offset = local_offset - cached.range.start;
        self.current_file
            .as_mut()
            .expect("selected archive range is open")
            .seek(SeekFrom::Start(segment_offset))?;
        Ok((cached.range.start, cached.range.end))
    }
}

impl Clone for MultiVolumeStream {
    fn clone(&self) -> Self {
        Self {
            layout: self.layout.clone(),
            current_path: None,
            current_file: None,
            current_range: 0..0,
            position: self.position,
        }
    }
}

impl Read for MultiVolumeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() || self.position == self.layout.total_size {
            return Ok(0);
        }
        let index = self
            .layout
            .layouts
            .partition_point(|volume| volume.end <= self.position);
        let (volume_start, volume_end) = {
            let volume = self.layout.layouts.get(index).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "archive stream ended")
            })?;
            (volume.start, volume.end)
        };
        let local_offset = self.position - volume_start;
        let (_, segment_end) = self.select_segment(index, local_offset)?;
        let available = segment_end
            .saturating_sub(local_offset)
            .min(volume_end.saturating_sub(self.position));
        let limit = usize::try_from(available)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let read = self
            .current_file
            .as_mut()
            .expect("selected archive range is open")
            .read(&mut buf[..limit])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "archive range cache ended early",
            ));
        }
        self.position = self.position.saturating_add(read as u64);
        Ok(read)
    }
}

impl Seek for MultiVolumeStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.layout.total_size) + i128::from(offset),
        };
        if target < 0 || target > i128::from(self.layout.total_size) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek beyond archive bounds",
            ));
        }
        self.position = u64::try_from(target).expect("validated range fits in u64");
        Ok(self.position)
    }
}

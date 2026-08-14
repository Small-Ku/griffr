use std::fmt::Display;
use std::path::Path;
use std::time::Duration;

use compio::buf::BufResult;
use compio::bytes::{Bytes, BytesMut};
use compio::io::AsyncWriteAtExt;
use futures_util::{Stream, StreamExt};

use crate::error::{Error, Result};

const WRITE_BATCH_BYTES: usize = 1024 * 1024;
const WRITE_QUEUE_DEPTH: usize = 2;

/// Streams one HTTP body into a file through a small bounded queue. The
/// producer can receive and inspect the next body chunks while the consumer
/// writes the previous batch, without allowing unbounded response buffering.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_http_body<S, E, C, W>(
    stream: S,
    file: compio::fs::File,
    path: &Path,
    item: &str,
    start_offset: u64,
    body_timeout: Duration,
    mut on_chunk: C,
    mut on_written: W,
) -> Result<(compio::fs::File, u64)>
where
    S: Stream<Item = std::result::Result<Bytes, E>>,
    E: Display,
    C: FnMut(&[u8]),
    W: FnMut(u64),
{
    let (write_tx, write_rx) = flume::bounded::<(Bytes, u64)>(WRITE_QUEUE_DEPTH);
    let producer = async move {
        let mut stream = Box::pin(stream);
        let mut write_offset = start_offset;
        let mut batch = BytesMut::with_capacity(WRITE_BATCH_BYTES);

        loop {
            let next = compio::time::timeout(body_timeout, stream.next())
                .await
                .map_err(|_| Error::Message {
                    context: "Download error: ",
                    detail: format!(
                        "Timed out reading response body from {item} (timeout={}s)",
                        body_timeout.as_secs()
                    ),
                })?;
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk.map_err(|source| Error::Message {
                context: "Download error: ",
                detail: format!("Failed to read response body from {item}: {source}"),
            })?;
            on_chunk(chunk.as_ref());

            if !batch.is_empty() && batch.len().saturating_add(chunk.len()) > WRITE_BATCH_BYTES {
                let ready =
                    std::mem::replace(&mut batch, BytesMut::with_capacity(WRITE_BATCH_BYTES))
                        .freeze();
                let ready_len = ready.len() as u64;
                write_tx
                    .send_async((ready, write_offset))
                    .await
                    .map_err(|_| writer_stopped(item))?;
                write_offset = write_offset.saturating_add(ready_len);
            }

            if batch.is_empty() && chunk.len() >= WRITE_BATCH_BYTES {
                let chunk_len = chunk.len() as u64;
                write_tx
                    .send_async((chunk, write_offset))
                    .await
                    .map_err(|_| writer_stopped(item))?;
                write_offset = write_offset.saturating_add(chunk_len);
            } else {
                batch.extend_from_slice(chunk.as_ref());
            }
        }

        if !batch.is_empty() {
            let ready = batch.freeze();
            let ready_len = ready.len() as u64;
            write_tx
                .send_async((ready, write_offset))
                .await
                .map_err(|_| writer_stopped(item))?;
            write_offset = write_offset.saturating_add(ready_len);
        }
        Ok::<u64, Error>(write_offset)
    };

    let writer = async move {
        let mut file = file;
        while let Ok((chunk, write_offset)) = write_rx.recv_async().await {
            let chunk_len = chunk.len() as u64;
            let BufResult(write_result, _) = file.write_all_at(chunk, write_offset).await;
            write_result.map_err(|source| Error::IoAt {
                action: "write to file",
                path: path.to_path_buf(),
                source,
            })?;
            on_written(write_offset.saturating_add(chunk_len));
        }
        Ok::<compio::fs::File, Error>(file)
    };

    let (total_written, file) = futures_util::try_join!(producer, writer)?;
    Ok((file, total_written))
}

fn writer_stopped(item: &str) -> Error {
    Error::Message {
        context: "Download error: ",
        detail: format!("File writer stopped while downloading {item}"),
    }
}

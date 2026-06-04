use std::path::PathBuf;

use anyhow::Context;
use compio::dispatcher::Dispatcher;

use super::fs_ops::{
    commit_staged_extract, create_hardlink, make_extract_staging_dir, reuse_file, ReuseMethod,
};
use super::types::{ArchivePart, ProgressEvent, Task};

pub(crate) fn execute_task(
    task: Task,
    max_retries: u32,
    extraction_progress_buffer_bytes: usize,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    match task {
        Task::InstallArchive {
            source_dir,
            base_name,
            dest,
            cleanup,
            password,
            parts,
        } => execute_install_archive(
            source_dir,
            base_name,
            dest,
            cleanup,
            password,
            parts,
            max_retries,
            io_dispatcher,
            user_agent,
            spawned,
            events,
        ),
        Task::Verify {
            path,
            logical_path,
            expected_md5,
            expected_size,
            on_fail,
        } => super::verify::execute_verify(
            &path,
            &logical_path,
            &expected_md5,
            expected_size,
            on_fail,
            spawned,
            events,
        ),
        Task::Download {
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count,
        } => execute_download(
            DownloadExecInput {
                url,
                dest,
                logical_path,
                expected_md5,
                expected_size,
                retry_count,
                max_retries,
            },
            io_dispatcher,
            user_agent,
            spawned,
            events,
        ),
        Task::EnsureFile {
            dest,
            logical_path,
            expected_md5,
            expected_size,
            source_candidates,
            download_url,
            allow_copy_fallback,
            prefer_reuse,
            retry_count,
        } => execute_ensure_file(
            EnsureFileInput {
                dest,
                logical_path,
                expected_md5,
                expected_size,
                source_candidates,
                download_url,
                allow_copy_fallback,
                prefer_reuse,
                retry_count,
                max_retries,
            },
            io_dispatcher,
            user_agent,
            spawned,
            events,
        ),
        Task::Hardlink { src, dest } => match create_hardlink(io_dispatcher, &src, &dest) {
            Ok(()) => events.push(ProgressEvent::Hardlinked { path: dest }),
            Err(err) => events.push(ProgressEvent::Failed {
                path: dest.display().to_string(),
                reason: err.to_string(),
            }),
        },
        Task::Extract {
            source_dir,
            base_name,
            dest,
            cleanup,
            password,
        } => execute_extract_archive(
            source_dir,
            base_name,
            dest,
            cleanup,
            password,
            extraction_progress_buffer_bytes,
            events,
        ),
    }
}

fn execute_install_archive(
    source_dir: PathBuf,
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    parts: Vec<ArchivePart>,
    max_retries: u32,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    for part in parts {
        let mut completed = false;
        for attempt in 0..=max_retries {
            if super::verify::build_issue(
                &part.dest,
                &part.logical_path,
                &part.expected_md5,
                Some(part.expected_size),
            )
            .is_none()
            {
                events.push(ProgressEvent::Verified {
                    path: part.logical_path.clone(),
                    ok: true,
                    issue: None,
                });
                completed = true;
                break;
            }

            match super::download::do_download(
                io_dispatcher,
                user_agent,
                &part.url,
                &part.dest,
                &part.expected_md5,
                Some(part.expected_size),
            ) {
                Ok(bytes) => {
                    events.push(ProgressEvent::Downloaded {
                        path: part.logical_path.clone(),
                        bytes,
                    });
                    let post_issue = super::verify::build_issue(
                        &part.dest,
                        &part.logical_path,
                        &part.expected_md5,
                        Some(part.expected_size),
                    );
                    if post_issue.is_none() {
                        events.push(ProgressEvent::Verified {
                            path: part.logical_path.clone(),
                            ok: true,
                            issue: None,
                        });
                        completed = true;
                        break;
                    }
                    if attempt < max_retries {
                        events.push(ProgressEvent::Retried {
                            path: part.logical_path.clone(),
                            reason: format!(
                                "install-archive verify attempt {} failed",
                                attempt + 1
                            ),
                        });
                        continue;
                    }
                    events.push(ProgressEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue: post_issue,
                    });
                    events.push(ProgressEvent::Failed {
                        path: part.logical_path.clone(),
                        reason: "install-archive verify failed after retries".to_string(),
                    });
                    return;
                }
                Err(err) => {
                    if attempt < max_retries {
                        events.push(ProgressEvent::Retried {
                            path: part.logical_path.clone(),
                            reason: format!(
                                "install-archive download attempt {} failed: {}",
                                attempt + 1,
                                err
                            ),
                        });
                        continue;
                    }
                    let issue = super::verify::build_issue(
                        &part.dest,
                        &part.logical_path,
                        &part.expected_md5,
                        Some(part.expected_size),
                    );
                    events.push(ProgressEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue,
                    });
                    events.push(ProgressEvent::Failed {
                        path: part.logical_path.clone(),
                        reason: format!("install-archive download failed after retries: {}", err),
                    });
                    return;
                }
            }
        }
        if !completed {
            return;
        }
    }

    spawned.push(Task::Extract {
        source_dir,
        base_name,
        dest,
        cleanup,
        password,
    });
}

fn execute_extract_archive(
    source_dir: PathBuf,
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    password: Option<String>,
    extraction_progress_buffer_bytes: usize,
    events: &mut Vec<ProgressEvent>,
) {
    let extract_progress_events = std::cell::RefCell::new(Vec::<ProgressEvent>::new());
    let result =
        crate::download::extractor::MultiVolumeExtractor::from_directory(&source_dir, &base_name)
            .and_then(|extractor| {
                let staging_dir = make_extract_staging_dir(&dest, &base_name)?;
                std::fs::create_dir_all(&staging_dir).with_context(|| {
                    format!(
                        "Failed to create extraction staging dir {}",
                        staging_dir.display()
                    )
                })?;
                let progress_path = base_name.clone();
                if let Err(err) = extractor.extract_to_with_progress(
                    &staging_dir,
                    password.as_deref(),
                    extraction_progress_buffer_bytes,
                    Some(|bytes, total_bytes| {
                        extract_progress_events
                            .borrow_mut()
                            .push(ProgressEvent::ExtractedBytes {
                                path: progress_path.clone(),
                                bytes,
                                total_bytes,
                            });
                    }),
                ) {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(err);
                }
                if let Err(err) = commit_staged_extract(&staging_dir, &dest) {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(err);
                }
                if cleanup {
                    extractor.cleanup()?;
                }
                Ok(())
            });
    events.extend(extract_progress_events.into_inner());
    match result {
        Ok(()) => events.push(ProgressEvent::Extracted { path: dest }),
        Err(err) => events.push(ProgressEvent::Failed {
            path: format!("{}/{}", source_dir.display(), base_name),
            reason: err.to_string(),
        }),
    }
}

struct DownloadExecInput {
    url: String,
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: Option<u64>,
    retry_count: u32,
    max_retries: u32,
}

struct EnsureFileInput {
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    source_candidates: Vec<PathBuf>,
    download_url: Option<String>,
    allow_copy_fallback: bool,
    prefer_reuse: bool,
    retry_count: u32,
    max_retries: u32,
}

fn execute_download(
    input: DownloadExecInput,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    let result = super::download::do_download(
        io_dispatcher,
        user_agent,
        &input.url,
        &input.dest,
        &input.expected_md5,
        input.expected_size,
    );
    match result {
        Ok(bytes) => {
            events.push(ProgressEvent::Downloaded {
                path: input.logical_path.clone(),
                bytes,
            });
            let on_fail = if input.retry_count < input.max_retries {
                Some(Box::new(Task::Download {
                    url: input.url.clone(),
                    dest: input.dest.clone(),
                    logical_path: input.logical_path.clone(),
                    expected_md5: input.expected_md5.clone(),
                    expected_size: input.expected_size,
                    retry_count: input.retry_count + 1,
                }))
            } else {
                None
            };
            spawned.push(Task::Verify {
                path: input.dest,
                logical_path: input.logical_path,
                expected_md5: input.expected_md5,
                expected_size: input.expected_size,
                on_fail,
            });
        }
        Err(err) => {
            if input.retry_count < input.max_retries {
                events.push(ProgressEvent::Retried {
                    path: input.logical_path.clone(),
                    reason: format!("download attempt {} failed: {}", input.retry_count + 1, err),
                });
                spawned.push(Task::Download {
                    url: input.url,
                    dest: input.dest,
                    logical_path: input.logical_path,
                    expected_md5: input.expected_md5,
                    expected_size: input.expected_size,
                    retry_count: input.retry_count + 1,
                });
            } else {
                events.push(ProgressEvent::Failed {
                    path: input.logical_path.clone(),
                    reason: format!("download failed after retries: {}", err),
                });
                spawned.push(Task::Verify {
                    path: input.dest,
                    logical_path: input.logical_path,
                    expected_md5: input.expected_md5,
                    expected_size: input.expected_size,
                    on_fail: None,
                });
            }
        }
    }
}

fn execute_ensure_file(
    input: EnsureFileInput,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    let existing_ok = super::verify::build_issue(
        &input.dest,
        &input.logical_path,
        &input.expected_md5,
        Some(input.expected_size),
    )
    .is_none();
    if existing_ok && !input.prefer_reuse {
        events.push(ProgressEvent::Verified {
            path: input.logical_path,
            ok: true,
            issue: None,
        });
        return;
    }
    let mut reuse_error = None;
    for source in &input.source_candidates {
        if super::verify::build_issue(
            source,
            &input.logical_path,
            &input.expected_md5,
            Some(input.expected_size),
        )
        .is_some()
        {
            continue;
        }
        match reuse_file(
            io_dispatcher,
            source,
            &input.dest,
            input.allow_copy_fallback,
        ) {
            Ok(ReuseMethod::Hardlink) => {
                events.push(ProgressEvent::Hardlinked {
                    path: input.dest.clone(),
                });
                if super::verify::build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                    return;
                }
            }
            Ok(ReuseMethod::Copy) => {
                events.push(ProgressEvent::Copied {
                    path: input.dest.clone(),
                });
                if super::verify::build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                    return;
                }
            }
            Err(err) => reuse_error = Some(err.to_string()),
        }
    }
    if existing_ok {
        events.push(ProgressEvent::Verified {
            path: input.logical_path,
            ok: true,
            issue: None,
        });
        return;
    }
    if let Some(download_url) = &input.download_url {
        match super::download::do_download(
            io_dispatcher,
            user_agent,
            download_url,
            &input.dest,
            &input.expected_md5,
            Some(input.expected_size),
        ) {
            Ok(bytes) => {
                events.push(ProgressEvent::Downloaded {
                    path: input.logical_path.clone(),
                    bytes,
                });
                if super::verify::build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                } else {
                    let issue = super::verify::build_issue(
                        &input.dest,
                        &input.logical_path,
                        &input.expected_md5,
                        Some(input.expected_size),
                    );
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path.clone(),
                        ok: false,
                        issue,
                    });
                }
                return;
            }
            Err(err) if input.retry_count < input.max_retries => {
                events.push(ProgressEvent::Retried {
                    path: input.logical_path.clone(),
                    reason: format!(
                        "ensure-file download attempt {} failed: {}",
                        input.retry_count + 1,
                        err
                    ),
                });
                spawned.push(Task::EnsureFile {
                    dest: input.dest,
                    logical_path: input.logical_path,
                    expected_md5: input.expected_md5,
                    expected_size: input.expected_size,
                    source_candidates: input.source_candidates,
                    download_url: input.download_url,
                    allow_copy_fallback: input.allow_copy_fallback,
                    prefer_reuse: input.prefer_reuse,
                    retry_count: input.retry_count + 1,
                });
                return;
            }
            Err(err) => reuse_error = Some(err.to_string()),
        }
    }
    let issue = super::verify::build_issue(
        &input.dest,
        &input.logical_path,
        &input.expected_md5,
        Some(input.expected_size),
    );
    events.push(ProgressEvent::Verified {
        path: input.logical_path.clone(),
        ok: false,
        issue,
    });
    events.push(ProgressEvent::Failed {
        path: input.logical_path,
        reason: reuse_error.unwrap_or_else(|| "ensure-file failed".to_string()),
    });
}

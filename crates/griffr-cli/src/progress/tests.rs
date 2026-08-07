use super::*;
fn hide_group(bar: &StepProgress) {
    bar.multi
        .as_ref()
        .expect("grouped progress")
        .set_draw_target(ProgressDrawTarget::hidden());
}

#[test]
fn count_and_byte_progress_keep_independent_units() {
    let progress = CountAndByteProgress::new("verify", "repair.download", false);
    hide_group(&progress.count);
    let count_lane = ProgressLane::INTEGRITY_VERIFY;
    let byte_lane = ProgressLane::INTEGRITY_DOWNLOAD;
    let session = progress.start(count_lane, byte_lane);
    let sender = session.sender();

    sender.emit(ProgressUpdate::Started {
        lane: count_lane,
        unit: ProgressUnit::Items,
        total: Some(10),
    });
    sender.emit(ProgressUpdate::Advanced {
        lane: count_lane,
        finished: 1,
        total: Some(10),
        item: Some("a.bin".to_string()),
    });
    sender.emit(ProgressUpdate::Started {
        lane: byte_lane,
        unit: ProgressUnit::Bytes,
        total: Some(128),
    });
    sender.emit(ProgressUpdate::Advanced {
        lane: byte_lane,
        finished: 64,
        total: Some(128),
        item: Some("b.bin".to_string()),
    });
    drop(sender);
    session.finish();

    assert_eq!(progress.count.bar.position(), 1);
    assert_eq!(progress.count.bar.length(), Some(10));
    assert_eq!(progress.bytes.bar.position(), 64);
    assert_eq!(progress.bytes.bar.length(), Some(128));
}

#[test]
fn archive_progress_keeps_download_and_extract_separate() {
    let progress = ArchiveProgress::new("install", false);
    hide_group(&progress.part_count);
    let verify_lane = ProgressLane::ARCHIVE_VERIFY;
    let download_lane = ProgressLane::ARCHIVE_DOWNLOAD;
    let extract_lane = ProgressLane::ARCHIVE_EXTRACT;
    let commit_lane = ProgressLane::ARCHIVE_COMMIT;
    let patch_lane = ProgressLane::ARCHIVE_PATCH;
    let delete_lane = ProgressLane::ARCHIVE_DELETE;
    let session = progress.start(
        verify_lane,
        download_lane,
        extract_lane,
        commit_lane,
        patch_lane,
        delete_lane,
    );
    let sender = session.sender();

    sender.emit(ProgressUpdate::Advanced {
        lane: download_lane,
        finished: 50,
        total: Some(100),
        item: Some("pack.001".to_string()),
    });
    sender.emit(ProgressUpdate::Advanced {
        lane: extract_lane,
        finished: 20,
        total: Some(200),
        item: Some("pack".to_string()),
    });
    drop(sender);
    session.finish();

    assert_eq!(progress.download.bar.position(), 50);
    assert_eq!(progress.download.bar.length(), Some(100));
    assert_eq!(progress.extract.bar.position(), 20);
    assert_eq!(progress.extract.bar.length(), Some(200));
}

#[test]
fn incomplete_progress_uses_floor_percentage() {
    assert_eq!(display_percent(13_310, 13_315), 99);
    assert_eq!(display_percent(13_314, 13_315), 99);
    assert_eq!(display_percent(13_315, 13_315), 100);
}

#[test]
fn rendered_prefix_never_rounds_an_incomplete_count_to_100() {
    let progress = StepProgress::new("verify", false);
    progress.update_count(13_310, 13_315, "");
    assert_eq!(progress.bar.position(), 13_310);
    assert_eq!(progress.bar.length(), Some(13_315));
    assert_eq!(progress.bar.prefix(), " 99%");

    progress.update_count(13_315, 13_315, "");
    assert_eq!(progress.bar.prefix(), "100%");
}

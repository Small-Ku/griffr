use super::*;
use crate::runtime::task_pool::TaskProgress;
use crate::runtime::{ProgressLane, ProgressSender, ProgressUnit, ProgressUpdate};

#[test]
fn disabled_task_progress_does_not_configure_lanes() {
    let lane = ProgressLane::INTEGRITY_VERIFY;
    let progress = TaskProgress::new(ProgressSender::disabled()).with_verify(lane, 2);

    assert!(progress.verify.is_none());
}

#[test]
fn worker_events_store_only_durable_task_outcomes() {
    let mut reducer = TaskProgressReducer::new(TaskProgress::disabled());
    let mut outcomes = Vec::new();

    record_worker_event(
        &mut reducer,
        &mut outcomes,
        WorkerEvent::progress(
            crate::runtime::ProgressPhase::Download,
            "a.bin".to_string(),
            32,
            64,
            false,
        ),
    );
    record_worker_event(
        &mut reducer,
        &mut outcomes,
        WorkerEvent::Retried {
            path: "a.bin".to_string(),
            reason: "checksum mismatch".to_string(),
        },
    );
    assert!(outcomes.is_empty());

    record_worker_event(
        &mut reducer,
        &mut outcomes,
        WorkerEvent::downloaded("a.bin".to_string(), 64),
    );
    assert!(matches!(
        outcomes.as_slice(),
        [TaskOutcome::Downloaded { path, bytes: 64 }] if path == "a.bin"
    ));
}

#[test]
fn reducer_emits_scoped_updates_without_regressing_retry_bytes() {
    let verify_lane = ProgressLane::INTEGRITY_VERIFY;
    let download_lane = ProgressLane::INTEGRITY_DOWNLOAD;
    let (sender, receiver) = ProgressSender::channel();
    let mut reducer = TaskProgressReducer::new(
        TaskProgress::new(sender)
            .with_verify(verify_lane, 2)
            .with_download(download_lane),
    );

    reducer.handle(&WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        "a.bin".to_string(),
        0,
        100,
        false,
    ));
    reducer.handle(&WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        "a.bin".to_string(),
        90,
        100,
        false,
    ));
    reducer.handle(&WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        "a.bin".to_string(),
        10,
        100,
        false,
    ));
    reducer.handle(&WorkerEvent::verified("a.bin".to_string(), true, None));
    reducer.finish();
    drop(reducer);

    let mut updates = Vec::new();
    while let Some(update) = receiver.try_recv() {
        updates.push(update);
    }

    assert!(updates.contains(&ProgressUpdate::Started {
        lane: verify_lane,
        unit: ProgressUnit::Items,
        total: Some(2),
    }));
    assert!(updates.contains(&ProgressUpdate::Advanced {
        lane: verify_lane,
        finished: 1,
        total: Some(2),
        item: Some("a.bin".to_string()),
    }));
    let downloaded_positions = updates
        .iter()
        .filter_map(|update| match update {
            ProgressUpdate::Advanced { lane, finished, .. } if *lane == download_lane => {
                Some(*finished)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(downloaded_positions.last().copied(), Some(90));
}

#[test]
fn reducer_accepts_explicit_download_reset_after_restart() {
    let lane = ProgressLane::INTEGRITY_DOWNLOAD;
    let (sender, receiver) = ProgressSender::channel();
    let mut reducer = TaskProgressReducer::new(TaskProgress::new(sender).with_download(lane));

    reducer.handle(&WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        "a.bin".to_string(),
        0,
        100,
        false,
    ));
    reducer.handle(&WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        "a.bin".to_string(),
        80,
        100,
        false,
    ));
    reducer.handle(&WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        "a.bin".to_string(),
        0,
        0,
        true,
    ));
    reducer.handle(&WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        "a.bin".to_string(),
        20,
        100,
        false,
    ));

    let mut positions = Vec::new();
    while let Some(update) = receiver.try_recv() {
        if let ProgressUpdate::Advanced {
            lane: update_lane,
            finished,
            ..
        } = update
        {
            if update_lane == lane {
                positions.push(finished);
            }
        }
    }
    assert!(positions.windows(2).any(|pair| pair == [80, 0]));
    assert_eq!(positions.last().copied(), Some(20));
}

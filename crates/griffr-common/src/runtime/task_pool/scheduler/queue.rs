use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use crate::error::{Error, Result};

use super::TaskPriority;
use crate::runtime::task_pool::Task;

const CONTINUATION_BURST: usize = 3;

#[derive(Debug, Default)]
struct QueueState {
    continuation: VecDeque<Task>,
    bulk: VecDeque<Task>,
    continuation_streak: usize,
}

#[derive(Debug, Default)]
pub(super) struct WorkerQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl WorkerQueue {
    pub(super) fn push(
        &self,
        task: Task,
        priority: TaskPriority,
        shutdown: &AtomicBool,
    ) -> Result<()> {
        if shutdown.load(Ordering::Acquire) {
            return Err(Error::TaskPool(
                "Failed to enqueue task: task pool is shutting down".to_string(),
            ));
        }
        let mut state = self.state.lock().unwrap();
        match priority {
            TaskPriority::Continuation => state.continuation.push_back(task),
            TaskPriority::Bulk => state.bulk.push_back(task),
        }
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    pub(super) fn pop(&self, shutdown: &AtomicBool) -> Option<Task> {
        let mut state = self.state.lock().unwrap();
        loop {
            if shutdown.load(Ordering::Acquire) {
                return None;
            }

            let force_bulk = !state.bulk.is_empty()
                && state.continuation_streak >= CONTINUATION_BURST;
            if force_bulk {
                state.continuation_streak = 0;
                return state.bulk.pop_front();
            }
            if let Some(task) = state.continuation.pop_front() {
                state.continuation_streak = state.continuation_streak.saturating_add(1);
                return Some(task);
            }
            if let Some(task) = state.bulk.pop_front() {
                state.continuation_streak = 0;
                return Some(task);
            }

            state = self.ready.wait(state).unwrap();
        }
    }

    pub(super) fn notify_all(&self) {
        self.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerQueue;
    use crate::runtime::task_pool::scheduler::TaskPriority;
    use crate::runtime::task_pool::Task;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn hardlink(name: &str) -> Task {
        Task::Hardlink {
            src: PathBuf::from(format!("{name}.src")),
            dest: PathBuf::from(name),
        }
    }

    fn destination(task: Task) -> PathBuf {
        match task {
            Task::Hardlink { dest, .. } => dest,
            _ => panic!("unexpected task"),
        }
    }

    #[test]
    fn continuations_run_first_without_starving_bulk_work() {
        let queue = WorkerQueue::default();
        let shutdown = AtomicBool::new(false);
        queue
            .push(hardlink("bulk"), TaskPriority::Bulk, &shutdown)
            .unwrap();
        for name in ["c1", "c2", "c3", "c4"] {
            queue
                .push(hardlink(name), TaskPriority::Continuation, &shutdown)
                .unwrap();
        }

        assert_eq!(destination(queue.pop(&shutdown).unwrap()), PathBuf::from("c1"));
        assert_eq!(destination(queue.pop(&shutdown).unwrap()), PathBuf::from("c2"));
        assert_eq!(destination(queue.pop(&shutdown).unwrap()), PathBuf::from("c3"));
        assert_eq!(destination(queue.pop(&shutdown).unwrap()), PathBuf::from("bulk"));
        assert_eq!(destination(queue.pop(&shutdown).unwrap()), PathBuf::from("c4"));
    }
}

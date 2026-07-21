use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use super::routing::ResourceRequest;
use crate::runtime::task_pool::{TaskPoolMetrics, VolumeTaskMetrics};

#[derive(Debug, Clone)]
struct TimingSample {
    queue_wait: Duration,
    run_time: Duration,
    estimated_bytes: u64,
    read_volumes: Vec<String>,
    write_volumes: Vec<String>,
    metadata_volumes: Vec<String>,
}

#[derive(Debug, Default)]
pub(super) struct SchedulerMetrics {
    samples: Mutex<Vec<TimingSample>>,
}

impl SchedulerMetrics {
    pub(super) fn record(
        &self,
        queue_wait: Duration,
        run_time: Duration,
        resources: &ResourceRequest,
    ) {
        self.samples.lock().unwrap().push(TimingSample {
            queue_wait,
            run_time,
            estimated_bytes: resources.estimated_bytes,
            read_volumes: resources.read_volumes.clone(),
            write_volumes: resources.write_volumes.clone(),
            metadata_volumes: resources.metadata_volumes.clone(),
        });
    }

    pub(super) fn snapshot(&self) -> TaskPoolMetrics {
        let samples = self.samples.lock().unwrap();
        let mut queue_waits = samples
            .iter()
            .map(|sample| sample.queue_wait)
            .collect::<Vec<_>>();
        let mut run_times = samples
            .iter()
            .map(|sample| sample.run_time)
            .collect::<Vec<_>>();
        let mut volumes = BTreeMap::<String, VolumeTaskMetrics>::new();
        for sample in samples.iter() {
            for volume in &sample.read_volumes {
                let metric = volumes.entry(volume.clone()).or_default();
                metric.read_bytes = metric.read_bytes.saturating_add(sample.estimated_bytes);
                metric.read_service_time = metric.read_service_time.saturating_add(sample.run_time);
                metric.read_tasks = metric.read_tasks.saturating_add(1);
            }
            for volume in &sample.write_volumes {
                let metric = volumes.entry(volume.clone()).or_default();
                metric.write_bytes = metric.write_bytes.saturating_add(sample.estimated_bytes);
                metric.write_service_time =
                    metric.write_service_time.saturating_add(sample.run_time);
                metric.write_tasks = metric.write_tasks.saturating_add(1);
            }
            for volume in &sample.metadata_volumes {
                let metric = volumes.entry(volume.clone()).or_default();
                metric.metadata_service_time =
                    metric.metadata_service_time.saturating_add(sample.run_time);
                metric.metadata_tasks = metric.metadata_tasks.saturating_add(1);
            }
        }
        TaskPoolMetrics {
            finished_tasks: samples.len(),
            graph: Default::default(),
            queue_wait_p50: percentile(&mut queue_waits, 50),
            queue_wait_p95: percentile(&mut queue_waits, 95),
            task_duration_p50: percentile(&mut run_times, 50),
            task_duration_p95: percentile(&mut run_times, 95),
            volumes,
        }
    }
}

fn percentile(samples: &mut [Duration], percentile: usize) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    samples.sort_unstable();
    let rank = (samples.len() * percentile).div_ceil(100);
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::task_pool::scheduler::routing::{ResourceRequest, RunClass};

    #[test]
    fn snapshot_reports_percentiles_and_volume_service_bytes() {
        let metrics = SchedulerMetrics::default();
        let resources = ResourceRequest {
            run: RunClass::Cpu,
            read_volumes: vec!["volume-a".to_string()],
            estimated_bytes: 1024,
            ..ResourceRequest::default()
        };
        metrics.record(
            Duration::from_millis(10),
            Duration::from_millis(20),
            &resources,
        );
        metrics.record(
            Duration::from_millis(30),
            Duration::from_millis(40),
            &resources,
        );

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.finished_tasks, 2);
        assert_eq!(snapshot.queue_wait_p50, Duration::from_millis(10));
        assert_eq!(snapshot.queue_wait_p95, Duration::from_millis(30));
        assert_eq!(snapshot.volumes["volume-a"].read_bytes, 2048);
    }
}

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub jobs_submitted: u64,
    pub tasks_enqueued: u64,
    pub tasks_started: u64,
    pub tasks_persisted: u64,
    pub tasks_rejected: u64,
    pub samples_persisted: u64,
    pub validator_pass: u64,
    pub validator_fail: u64,
}

pub trait Metrics: Send + Sync {
    fn inc_job_submitted(&self);
    fn inc_task_enqueued(&self);
    fn inc_task_started(&self);
    fn inc_task_persisted(&self);
    fn inc_task_rejected(&self);
    fn inc_samples_persisted(&self);
    fn record_validator_pass(&self);
    fn record_validator_fail(&self);
    fn snapshot(&self) -> MetricsSnapshot;
}

pub struct InMemoryMetrics {
    jobs_submitted: AtomicU64,
    tasks_enqueued: AtomicU64,
    tasks_started: AtomicU64,
    tasks_persisted: AtomicU64,
    tasks_rejected: AtomicU64,
    samples_persisted: AtomicU64,
    validator_pass: AtomicU64,
    validator_fail: AtomicU64,
}

impl InMemoryMetrics {
    pub fn new() -> Self {
        Self {
            jobs_submitted: AtomicU64::new(0),
            tasks_enqueued: AtomicU64::new(0),
            tasks_started: AtomicU64::new(0),
            tasks_persisted: AtomicU64::new(0),
            tasks_rejected: AtomicU64::new(0),
            samples_persisted: AtomicU64::new(0),
            validator_pass: AtomicU64::new(0),
            validator_fail: AtomicU64::new(0),
        }
    }
}

impl Default for InMemoryMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics for InMemoryMetrics {
    fn inc_job_submitted(&self) {
        self.jobs_submitted.fetch_add(1, Ordering::Relaxed);
    }
    fn inc_task_enqueued(&self) {
        self.tasks_enqueued.fetch_add(1, Ordering::Relaxed);
    }
    fn inc_task_started(&self) {
        self.tasks_started.fetch_add(1, Ordering::Relaxed);
    }
    fn inc_task_persisted(&self) {
        self.tasks_persisted.fetch_add(1, Ordering::Relaxed);
    }
    fn inc_task_rejected(&self) {
        self.tasks_rejected.fetch_add(1, Ordering::Relaxed);
    }
    fn inc_samples_persisted(&self) {
        self.samples_persisted.fetch_add(1, Ordering::Relaxed);
    }
    fn record_validator_pass(&self) {
        self.validator_pass.fetch_add(1, Ordering::Relaxed);
    }
    fn record_validator_fail(&self) {
        self.validator_fail.fetch_add(1, Ordering::Relaxed);
    }
    fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            jobs_submitted: self.jobs_submitted.load(Ordering::Relaxed),
            tasks_enqueued: self.tasks_enqueued.load(Ordering::Relaxed),
            tasks_started: self.tasks_started.load(Ordering::Relaxed),
            tasks_persisted: self.tasks_persisted.load(Ordering::Relaxed),
            tasks_rejected: self.tasks_rejected.load(Ordering::Relaxed),
            samples_persisted: self.samples_persisted.load(Ordering::Relaxed),
            validator_pass: self.validator_pass.load(Ordering::Relaxed),
            validator_fail: self.validator_fail.load(Ordering::Relaxed),
        }
    }
}

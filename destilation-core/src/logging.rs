use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEvent {
    pub ts: DateTime<Utc>,
    pub level: LogLevel,
    pub job_id: Option<String>,
    pub task_id: Option<String>,
    pub dataset_dir: Option<String>,
    pub message: String,
    pub fields: HashMap<String, String>,
}

pub trait EventLogger: Send + Sync {
    fn log(&self, event: LogEvent);
}

#[derive(Default)]
pub struct NoopEventLogger;

impl EventLogger for NoopEventLogger {
    fn log(&self, _event: LogEvent) {}
}

pub type SharedEventLogger = Arc<dyn EventLogger>;

pub struct BufferedFileEventLogger {
    seq: AtomicU64,
    max_events: usize,
    max_events_per_job: usize,
    state: Mutex<BufferedFileEventLoggerState>,
}

struct BufferedFileEventLoggerState {
    events: VecDeque<(u64, LogEvent)>,
    job_events: HashMap<String, VecDeque<(u64, LogEvent)>>,
}

impl BufferedFileEventLogger {
    pub fn new(max_events: usize, max_events_per_job: usize) -> Self {
        Self {
            seq: AtomicU64::new(0),
            max_events: max_events.max(1),
            max_events_per_job: max_events_per_job.max(1),
            state: Mutex::new(BufferedFileEventLoggerState {
                events: VecDeque::new(),
                job_events: HashMap::new(),
            }),
        }
    }

    pub fn events_since(&self, last_seq: u64) -> (u64, Vec<LogEvent>) {
        let state = self.state.lock().unwrap();
        let mut out = Vec::new();
        let mut new_last = last_seq;
        for (seq, ev) in state.events.iter() {
            if *seq > last_seq {
                out.push(ev.clone());
                new_last = new_last.max(*seq);
            }
        }
        (new_last, out)
    }

    pub fn job_events_tail(&self, job_id: &str, max: usize) -> Vec<LogEvent> {
        let state = self.state.lock().unwrap();
        let Some(q) = state.job_events.get(job_id) else {
            return Vec::new();
        };
        q.iter()
            .rev()
            .take(max)
            .cloned()
            .map(|(_, ev)| ev)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn event_path(event: &LogEvent) -> Option<PathBuf> {
        let dataset_dir = event.dataset_dir.as_ref()?;
        let job_id = event.job_id.as_ref()?;
        let dir = Path::new(dataset_dir);
        Some(dir.join(format!("{job_id}.events.jsonl")))
    }

    fn write_to_file(event: &LogEvent) {
        let Some(path) = Self::event_path(event) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        let line = line + "\n";
        let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        else {
            return;
        };
        let _ = std::io::Write::write_all(&mut f, line.as_bytes());
    }
}

impl EventLogger for BufferedFileEventLogger {
    fn log(&self, event: LogEvent) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;

        Self::write_to_file(&event);

        let mut state = self.state.lock().unwrap();
        state.events.push_back((seq, event.clone()));
        while state.events.len() > self.max_events {
            state.events.pop_front();
        }

        if let Some(job_id) = event.job_id.clone() {
            let q = state.job_events.entry(job_id).or_default();
            q.push_back((seq, event));
            while q.len() > self.max_events_per_job {
                q.pop_front();
            }
        }
    }
}

impl LogEvent {
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            ts: Utc::now(),
            level,
            job_id: None,
            task_id: None,
            dataset_dir: None,
            message: message.into(),
            fields: HashMap::new(),
        }
    }

    pub fn with_job(mut self, job_id: impl Into<String>) -> Self {
        self.job_id = Some(job_id.into());
        self
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_dataset_dir(mut self, dataset_dir: impl Into<String>) -> Self {
        self.dataset_dir = Some(dataset_dir.into());
        self
    }

    pub fn with_field(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.fields.insert(k.into(), v.into());
        self
    }
}

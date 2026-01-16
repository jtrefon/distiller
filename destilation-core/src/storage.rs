use crate::domain::{Job, JobConfig, JobId, JobStatus, Sample, Task, TaskState};
use async_trait::async_trait;
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::{Arc, Mutex};

#[async_trait]
pub trait JobStore: Send + Sync {
    async fn create_job(&self, config: JobConfig) -> anyhow::Result<Job>;
    async fn get_job(&self, id: &JobId) -> anyhow::Result<Option<Job>>;
    async fn update_job(&self, job: &Job) -> anyhow::Result<()>;
    async fn list_jobs(&self) -> anyhow::Result<Vec<Job>>;
    async fn delete_job(&self, id: &JobId) -> anyhow::Result<()>;
}

#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn enqueue_task(&self, task: Task) -> anyhow::Result<()>;
    async fn fetch_next_task(&self) -> anyhow::Result<Option<Task>>;
    async fn update_task(&self, task: &Task) -> anyhow::Result<()>;
    async fn list_tasks(&self, job_id: &JobId) -> anyhow::Result<Vec<Task>>;
    async fn delete_tasks_by_job(&self, job_id: &JobId) -> anyhow::Result<()>;
}

#[async_trait]
pub trait DatasetWriter: Send + Sync {
    async fn persist_sample(&self, sample: Sample) -> anyhow::Result<()>;
    async fn flush(&self) -> anyhow::Result<()>;
}

// --- In-Memory Implementations ---

pub struct InMemoryJobStore {
    inner: Arc<Mutex<HashMap<JobId, Job>>>,
}

impl InMemoryJobStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryJobStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl JobStore for InMemoryJobStore {
    async fn create_job(&self, config: JobConfig) -> anyhow::Result<Job> {
        let job = Job {
            id: config.id.clone(),
            config,
            status: JobStatus::Pending,
            created_at: chrono::Utc::now(),
            started_at: None,
            updated_at: chrono::Utc::now(),
            finished_at: None,
            completed_samples: 0,
        };
        self.inner
            .lock()
            .unwrap()
            .insert(job.id.clone(), job.clone());
        Ok(job)
    }

    async fn get_job(&self, id: &JobId) -> anyhow::Result<Option<Job>> {
        Ok(self.inner.lock().unwrap().get(id).cloned())
    }

    async fn update_job(&self, job: &Job) -> anyhow::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert(job.id.clone(), job.clone());
        Ok(())
    }

    async fn list_jobs(&self) -> anyhow::Result<Vec<Job>> {
        Ok(self.inner.lock().unwrap().values().cloned().collect())
    }

    async fn delete_job(&self, id: &JobId) -> anyhow::Result<()> {
        self.inner.lock().unwrap().remove(id);
        Ok(())
    }
}

pub struct InMemoryTaskStore {
    inner: Arc<Mutex<VecDeque<Task>>>,
    states: Arc<Mutex<HashMap<String, Task>>>,
}

impl InMemoryTaskStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryTaskStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn enqueue_task(&self, task: Task) -> anyhow::Result<()> {
        self.inner.lock().unwrap().push_back(task.clone());
        self.states.lock().unwrap().insert(task.id.clone(), task);
        Ok(())
    }

    async fn fetch_next_task(&self) -> anyhow::Result<Option<Task>> {
        Ok(self.inner.lock().unwrap().pop_front())
    }

    async fn update_task(&self, task: &Task) -> anyhow::Result<()> {
        self.states
            .lock()
            .unwrap()
            .insert(task.id.clone(), task.clone());
        Ok(())
    }

    async fn list_tasks(&self, job_id: &JobId) -> anyhow::Result<Vec<Task>> {
        let tasks: Vec<Task> = self
            .states
            .lock()
            .unwrap()
            .values()
            .filter(|t| &t.job_id == job_id)
            .cloned()
            .collect();
        Ok(tasks)
    }

    async fn delete_tasks_by_job(&self, job_id: &JobId) -> anyhow::Result<()> {
        let mut states = self.states.lock().unwrap();
        states.retain(|_, v| &v.job_id != job_id);

        let mut inner = self.inner.lock().unwrap();
        inner.retain(|t| &t.job_id != job_id);

        Ok(())
    }
}

pub struct FilesystemDatasetWriter {
    root: String,
}

impl FilesystemDatasetWriter {
    pub fn new(root: String) -> Self {
        Self { root }
    }
}

#[async_trait]
impl DatasetWriter for FilesystemDatasetWriter {
    async fn persist_sample(&self, sample: Sample) -> anyhow::Result<()> {
        let dir = std::path::Path::new(&self.root).join(&sample.job_id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("samples.jsonl");
        let line = serde_json::to_string(&sample)? + "\n";
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())?;
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

// --- SQLite Implementations ---

pub struct SqliteJobStore {
    pool: SqlitePool,
}

impl SqliteJobStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn init(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                config TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                started_at TEXT,
                updated_at TEXT NOT NULL,
                finished_at TEXT,
                completed_samples INTEGER NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        let cols = sqlx::query("PRAGMA table_info(jobs)")
            .fetch_all(&self.pool)
            .await?;
        let mut has_started_at = false;
        let mut has_finished_at = false;
        for row in cols {
            let name: String = row.get("name");
            if name == "started_at" {
                has_started_at = true;
            }
            if name == "finished_at" {
                has_finished_at = true;
            }
        }
        if !has_started_at {
            sqlx::query("ALTER TABLE jobs ADD COLUMN started_at TEXT")
                .execute(&self.pool)
                .await?;
        }
        if !has_finished_at {
            sqlx::query("ALTER TABLE jobs ADD COLUMN finished_at TEXT")
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl JobStore for SqliteJobStore {
    async fn create_job(&self, config: JobConfig) -> anyhow::Result<Job> {
        let job = Job {
            id: config.id.clone(),
            config: config.clone(),
            status: JobStatus::Pending,
            created_at: chrono::Utc::now(),
            started_at: None,
            updated_at: chrono::Utc::now(),
            finished_at: None,
            completed_samples: 0,
        };

        let config_json = serde_json::to_string(&job.config)?;
        let status_str = serde_json::to_string(&job.status)?;
        let created_at = job.created_at.to_rfc3339();
        let started_at: Option<String> = job.started_at.map(|t| t.to_rfc3339());
        let updated_at = job.updated_at.to_rfc3339();
        let finished_at: Option<String> = job.finished_at.map(|t| t.to_rfc3339());

        sqlx::query(
            r#"
            INSERT INTO jobs (id, config, status, created_at, started_at, updated_at, finished_at, completed_samples)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&job.id)
        .bind(config_json)
        .bind(status_str)
        .bind(created_at)
        .bind(started_at)
        .bind(updated_at)
        .bind(finished_at)
        .bind(job.completed_samples as i64)
        .execute(&self.pool)
        .await?;

        Ok(job)
    }

    async fn get_job(&self, id: &JobId) -> anyhow::Result<Option<Job>> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        if let Some(row) = row {
            let config_json: String = row.get("config");
            let status_str: String = row.get("status");
            let created_at_str: String = row.get("created_at");
            let started_at_str: Option<String> = row.try_get("started_at").unwrap_or(None);
            let updated_at_str: String = row.get("updated_at");
            let finished_at_str: Option<String> = row.try_get("finished_at").unwrap_or(None);
            let completed_samples: i64 = row.get("completed_samples");

            let config = serde_json::from_str(&config_json)?;
            let status = serde_json::from_str(&status_str)?;
            let created_at =
                chrono::DateTime::parse_from_rfc3339(&created_at_str)?.with_timezone(&chrono::Utc);
            let started_at = if let Some(s) = started_at_str {
                Some(chrono::DateTime::parse_from_rfc3339(&s)?.with_timezone(&chrono::Utc))
            } else {
                None
            };
            let updated_at =
                chrono::DateTime::parse_from_rfc3339(&updated_at_str)?.with_timezone(&chrono::Utc);
            let finished_at = if let Some(s) = finished_at_str {
                Some(chrono::DateTime::parse_from_rfc3339(&s)?.with_timezone(&chrono::Utc))
            } else {
                None
            };

            Ok(Some(Job {
                id: id.clone(),
                config,
                status,
                created_at,
                started_at,
                updated_at,
                finished_at,
                completed_samples: completed_samples as u64,
            }))
        } else {
            Ok(None)
        }
    }

    async fn update_job(&self, job: &Job) -> anyhow::Result<()> {
        let config_json = serde_json::to_string(&job.config)?;
        let status_str = serde_json::to_string(&job.status)?;
        let updated_at = job.updated_at.to_rfc3339();
        let started_at: Option<String> = job.started_at.map(|t| t.to_rfc3339());
        let finished_at: Option<String> = job.finished_at.map(|t| t.to_rfc3339());

        sqlx::query(
            r#"
            UPDATE jobs
            SET config = ?, status = ?, started_at = ?, updated_at = ?, finished_at = ?, completed_samples = ?
            WHERE id = ?
            "#,
        )
        .bind(config_json)
        .bind(status_str)
        .bind(started_at)
        .bind(updated_at)
        .bind(finished_at)
        .bind(job.completed_samples as i64)
        .bind(&job.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_jobs(&self) -> anyhow::Result<Vec<Job>> {
        let rows = sqlx::query("SELECT * FROM jobs")
            .fetch_all(&self.pool)
            .await?;

        let mut jobs = Vec::new();
        for row in rows {
            let id: String = row.get("id");
            let config_json: String = row.get("config");
            let status_str: String = row.get("status");
            let created_at_str: String = row.get("created_at");
            let started_at_str: Option<String> = row.try_get("started_at").unwrap_or(None);
            let updated_at_str: String = row.get("updated_at");
            let finished_at_str: Option<String> = row.try_get("finished_at").unwrap_or(None);
            let completed_samples: i64 = row.get("completed_samples");

            let config = serde_json::from_str(&config_json)?;
            let status = serde_json::from_str(&status_str)?;
            let created_at =
                chrono::DateTime::parse_from_rfc3339(&created_at_str)?.with_timezone(&chrono::Utc);
            let started_at = if let Some(s) = started_at_str {
                Some(chrono::DateTime::parse_from_rfc3339(&s)?.with_timezone(&chrono::Utc))
            } else {
                None
            };
            let updated_at =
                chrono::DateTime::parse_from_rfc3339(&updated_at_str)?.with_timezone(&chrono::Utc);
            let finished_at = if let Some(s) = finished_at_str {
                Some(chrono::DateTime::parse_from_rfc3339(&s)?.with_timezone(&chrono::Utc))
            } else {
                None
            };

            jobs.push(Job {
                id,
                config,
                status,
                created_at,
                started_at,
                updated_at,
                finished_at,
                completed_samples: completed_samples as u64,
            });
        }
        Ok(jobs)
    }

    async fn delete_job(&self, id: &JobId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

pub struct SqliteTaskStore {
    pool: SqlitePool,
}

impl SqliteTaskStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn init(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                state TEXT NOT NULL,
                attempts INTEGER NOT NULL,
                max_attempts INTEGER NOT NULL,
                provider_id TEXT,
                domain_id TEXT NOT NULL,
                template_id TEXT NOT NULL,
                prompt_spec TEXT NOT NULL,
                raw_response TEXT,
                validation_result TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl TaskStore for SqliteTaskStore {
    async fn enqueue_task(&self, task: Task) -> anyhow::Result<()> {
        let state_str = serde_json::to_string(&task.state)?;
        let prompt_spec = serde_json::to_string(&task.prompt_spec)?;
        let validation_result = if let Some(vr) = &task.validation_result {
            Some(serde_json::to_string(vr)?)
        } else {
            None
        };

        sqlx::query(
            r#"
            INSERT INTO tasks (
                id, job_id, state, attempts, max_attempts, provider_id, 
                domain_id, template_id, prompt_spec, raw_response, validation_result
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&task.id)
        .bind(&task.job_id)
        .bind(state_str)
        .bind(task.attempts as i64)
        .bind(task.max_attempts as i64)
        .bind(&task.provider_id)
        .bind(&task.domain_id)
        .bind(&task.template_id)
        .bind(prompt_spec)
        .bind(&task.raw_response)
        .bind(validation_result)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn fetch_next_task(&self) -> anyhow::Result<Option<Task>> {
        // Simple transaction to pick a queued task and mark it generating
        // Note: SQLite is single writer, so this transaction logic is simple but correct enough
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query("SELECT * FROM tasks WHERE state = ? LIMIT 1")
            .bind(serde_json::to_string(&TaskState::Queued)?)
            .fetch_optional(&mut *tx)
            .await?;

        if let Some(row) = row {
            let id: String = row.get("id");
            let job_id: String = row.get("job_id");
            let _state_str: String = row.get("state");
            let attempts: i64 = row.get("attempts");
            let max_attempts: i64 = row.get("max_attempts");
            let provider_id: Option<String> = row.get("provider_id");
            let domain_id: String = row.get("domain_id");
            let template_id: String = row.get("template_id");
            let prompt_spec_str: String = row.get("prompt_spec");
            let raw_response: Option<String> = row.get("raw_response");
            let validation_result_str: Option<String> = row.get("validation_result");

            let state = TaskState::Generating;
            let new_state_str = serde_json::to_string(&state)?;

            sqlx::query("UPDATE tasks SET state = ? WHERE id = ?")
                .bind(new_state_str)
                .bind(&id)
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;

            let prompt_spec = serde_json::from_str(&prompt_spec_str)?;
            let validation_result = if let Some(s) = validation_result_str {
                Some(serde_json::from_str(&s)?)
            } else {
                None
            };

            Ok(Some(Task {
                id,
                job_id,
                state,
                attempts: attempts as u32,
                max_attempts: max_attempts as u32,
                provider_id,
                domain_id,
                template_id,
                prompt_spec,
                raw_response,
                validation_result,
            }))
        } else {
            Ok(None)
        }
    }

    async fn update_task(&self, task: &Task) -> anyhow::Result<()> {
        let state_str = serde_json::to_string(&task.state)?;
        // prompt_spec is immutable, no need to update
        let validation_result = if let Some(vr) = &task.validation_result {
            Some(serde_json::to_string(vr)?)
        } else {
            None
        };

        sqlx::query(
            r#"
            UPDATE tasks
            SET state = ?, attempts = ?, provider_id = ?, raw_response = ?, validation_result = ?
            WHERE id = ?
            "#,
        )
        .bind(state_str)
        .bind(task.attempts as i64)
        .bind(&task.provider_id)
        .bind(&task.raw_response)
        .bind(validation_result)
        .bind(&task.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_tasks(&self, job_id: &JobId) -> anyhow::Result<Vec<Task>> {
        let rows = sqlx::query("SELECT * FROM tasks WHERE job_id = ?")
            .bind(job_id)
            .fetch_all(&self.pool)
            .await?;

        let mut tasks = Vec::new();
        for row in rows {
            let id: String = row.get("id");
            let job_id: String = row.get("job_id");
            let state_str: String = row.get("state");
            let attempts: i64 = row.get("attempts");
            let max_attempts: i64 = row.get("max_attempts");
            let provider_id: Option<String> = row.get("provider_id");
            let domain_id: String = row.get("domain_id");
            let template_id: String = row.get("template_id");
            let prompt_spec_str: String = row.get("prompt_spec");
            let raw_response: Option<String> = row.get("raw_response");
            let validation_result_str: Option<String> = row.get("validation_result");

            let state = serde_json::from_str(&state_str)?;
            let prompt_spec = serde_json::from_str(&prompt_spec_str)?;
            let validation_result = if let Some(s) = validation_result_str {
                Some(serde_json::from_str(&s)?)
            } else {
                None
            };

            tasks.push(Task {
                id,
                job_id,
                state,
                attempts: attempts as u32,
                max_attempts: max_attempts as u32,
                provider_id,
                domain_id,
                template_id,
                prompt_spec,
                raw_response,
                validation_result,
            });
        }
        Ok(tasks)
    }

    async fn delete_tasks_by_job(&self, job_id: &JobId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM tasks WHERE job_id = ?")
            .bind(job_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

use crate::domain::{Job, JobConfig, JobId, JobStatus, ProviderId, Sample, Task, TaskId, TemplateConfig, TemplateId};
use crate::provider::ProviderConfig;
use async_trait::async_trait;
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

fn atomic_write(path: &std::path::Path, content: &str) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("no parent dir"))?;
    std::fs::create_dir_all(parent)?;
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn enqueue_task(&self, task: Task) -> anyhow::Result<()>;
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

// --- Directory-Centric Implementations ---

pub trait ProviderStore: Send + Sync {
    fn save_provider(&self, provider: &ProviderConfig) -> anyhow::Result<()>;
    fn delete_provider(&self, id: &ProviderId) -> anyhow::Result<()>;
    fn list_providers(&self) -> anyhow::Result<Vec<ProviderConfig>>;
}

pub struct DirectoryProviderStore {
    root: std::path::PathBuf,
}

impl DirectoryProviderStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
        }
    }

    fn providers_path(&self) -> std::path::PathBuf {
        self.root.join("providers.json")
    }
}

impl ProviderStore for DirectoryProviderStore {
    fn save_provider(&self, provider: &ProviderConfig) -> anyhow::Result<()> {
        let mut providers = self.list_providers()?;
        providers.retain(|p| p.id() != provider.id());
        providers.push(provider.clone());
        let content = serde_json::to_string_pretty(&providers)?;
        atomic_write(&self.providers_path(), &content)?;
        Ok(())
    }

    fn delete_provider(&self, id: &ProviderId) -> anyhow::Result<()> {
        let mut providers = self.list_providers()?;
        providers.retain(|p| p.id() != id);
        let content = serde_json::to_string_pretty(&providers)?;
        atomic_write(&self.providers_path(), &content)?;
        Ok(())
    }

    fn list_providers(&self) -> anyhow::Result<Vec<ProviderConfig>> {
        let path = self.providers_path();
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(path)?;
        let providers = serde_json::from_str(&content)?;
        Ok(providers)
    }
}

pub struct DirectoryJobStore {
    root: std::path::PathBuf,
}

impl DirectoryJobStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
        }
    }

    fn job_path(&self, id: &JobId) -> std::path::PathBuf {
        self.root.join(id).join("job.json")
    }
}

#[async_trait]
impl JobStore for DirectoryJobStore {
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
        let path = self.job_path(&job.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&job)?;
        atomic_write(&path, &content)?;
        Ok(job)
    }

    async fn get_job(&self, id: &JobId) -> anyhow::Result<Option<Job>> {
        let path = self.job_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let job = serde_json::from_str(&content)?;
        Ok(Some(job))
    }

    async fn update_job(&self, job: &Job) -> anyhow::Result<()> {
        let path = self.job_path(&job.id);
        let content = serde_json::to_string_pretty(job)?;
        atomic_write(&path, &content)?;
        Ok(())
    }

    async fn list_jobs(&self) -> anyhow::Result<Vec<Job>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }
        let mut jobs = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.path().is_dir() {
                let job_id = entry.file_name().to_string_lossy().to_string();
                if let Some(job) = self.get_job(&job_id).await? {
                    jobs.push(job);
                }
            }
        }
        Ok(jobs)
    }

    async fn delete_job(&self, id: &JobId) -> anyhow::Result<()> {
        let dir = self.root.join(id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }
}

pub struct DirectoryTaskStore {
    root: std::path::PathBuf,
}

impl DirectoryTaskStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
        }
    }

    fn tasks_dir(&self, job_id: &JobId) -> std::path::PathBuf {
        self.root.join(job_id).join("tasks")
    }

    fn task_path(&self, job_id: &JobId, task_id: &TaskId) -> std::path::PathBuf {
        self.tasks_dir(job_id).join(format!("{}.json", task_id))
    }
}

#[async_trait]
impl TaskStore for DirectoryTaskStore {
    async fn enqueue_task(&self, task: Task) -> anyhow::Result<()> {
        let dir = self.tasks_dir(&task.job_id);
        std::fs::create_dir_all(&dir)?;
        let path = self.task_path(&task.job_id, &task.id);
        let content = serde_json::to_string_pretty(&task)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    async fn update_task(&self, task: &Task) -> anyhow::Result<()> {
        let path = self.task_path(&task.job_id, &task.id);
        let content = serde_json::to_string_pretty(task)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    async fn list_tasks(&self, job_id: &JobId) -> anyhow::Result<Vec<Task>> {
        let dir = self.tasks_dir(job_id);
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut tasks = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            if entry.path().is_file() && entry.path().extension().map_or(false, |e| e == "json") {
                let content = std::fs::read_to_string(entry.path())?;
                let task = serde_json::from_str(&content)?;
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    async fn delete_tasks_by_job(&self, job_id: &JobId) -> anyhow::Result<()> {
        let dir = self.tasks_dir(job_id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }
}


pub trait TemplateStore: Send + Sync {
    fn save_template(&self, template: &TemplateConfig) -> anyhow::Result<()>;
    fn delete_template(&self, id: &TemplateId) -> anyhow::Result<()>;
    fn get_template(&self, id: &TemplateId) -> anyhow::Result<Option<TemplateConfig>>;
    fn list_templates(&self) -> anyhow::Result<Vec<TemplateConfig>>;
}

pub struct DirectoryTemplateStore {
    root: std::path::PathBuf,
}

impl DirectoryTemplateStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
        }
    }

    fn templates_dir(&self) -> std::path::PathBuf {
        self.root.join("templates")
    }

    fn template_path(&self, id: &TemplateId) -> std::path::PathBuf {
        self.templates_dir().join(format!("{}.json", id))
    }
}

impl TemplateStore for DirectoryTemplateStore {
    fn save_template(&self, template: &TemplateConfig) -> anyhow::Result<()> {
        let path = self.template_path(&template.id);
        let content = serde_json::to_string_pretty(template)?;
        atomic_write(&path, &content)?;
        Ok(())
    }

    fn delete_template(&self, id: &TemplateId) -> anyhow::Result<()> {
        let path = self.template_path(id);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn get_template(&self, id: &TemplateId) -> anyhow::Result<Option<TemplateConfig>> {
        let path = self.template_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let template = serde_json::from_str(&content)?;
        Ok(Some(template))
    }

    fn list_templates(&self) -> anyhow::Result<Vec<TemplateConfig>> {
        let dir = self.templates_dir();
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut templates = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().is_file() && entry.path().extension().map_or(false, |e| e == "json") {
                let content = std::fs::read_to_string(entry.path())?;
                let template = serde_json::from_str(&content)?;
                templates.push(template);
            }
        }
        Ok(templates)
    }
}


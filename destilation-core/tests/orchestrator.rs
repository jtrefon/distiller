use async_trait::async_trait;
use destilation_core::domain::{
    JobConfig, JobOutputConfig, JobProviderSpec, JobValidationConfig, ReasoningMode,
    TemplateConfig, TemplateMode, TemplateSchema, TemplateSchemaField,
};
use destilation_core::logging::BufferedFileEventLogger;
use destilation_core::logging::NoopEventLogger;
use destilation_core::metrics::{InMemoryMetrics, Metrics};
use destilation_core::orchestrator::Orchestrator;
use destilation_core::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderConfig, ProviderMetadata,
};
use destilation_core::storage::{
    DatasetWriter, FilesystemDatasetWriter, InMemoryJobStore, InMemoryTaskStore, JobStore,
    TaskStore,
};
use destilation_core::validation::Validator;
use destilation_core::validators::StructuralValidator;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

struct DummyStore;

#[async_trait::async_trait]
impl destilation_core::storage::ProviderStore for DummyStore {
    fn save_provider(&self, _provider: &destilation_core::provider::ProviderConfig) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn delete_provider(&self, _id: &String) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn list_providers(&self) -> anyhow::Result<Vec<destilation_core::provider::ProviderConfig>> {
        unimplemented!()
    }
}

#[async_trait::async_trait]
impl destilation_core::storage::TemplateStore for DummyStore {
    fn save_template(&self, _template: &destilation_core::domain::TemplateConfig) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn delete_template(&self, _id: &String) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn get_template(&self, _id: &String) -> anyhow::Result<Option<destilation_core::domain::TemplateConfig>> {
        unimplemented!()
    }
    fn list_templates(&self) -> anyhow::Result<Vec<destilation_core::domain::TemplateConfig>> {
        unimplemented!()
    }
}

struct InMemoryDatasetWriter {
    persisted: Arc<std::sync::Mutex<Vec<destilation_core::domain::Sample>>>,
}

#[async_trait]
impl DatasetWriter for InMemoryDatasetWriter {
    async fn persist_sample(&self, sample: destilation_core::domain::Sample) -> anyhow::Result<()> {
        self.persisted.lock().unwrap().push(sample);
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct MockProvider;

#[async_trait]
impl ModelProvider for MockProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: "mock".to_string(),
            name: "Mock".to_string(),
            models: vec!["mock-model".to_string()],
            max_tokens: None,
            supports_tools: false,
            capabilities: vec![],
        }
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResult, destilation_core::provider::ProviderError> {
        Ok(GenerationResult {
            provider_id: request.provider_id,
            model: request.model,
            raw_output: serde_json::json!({"question":"Q","answer":"A"}).to_string(),
            latency: std::time::Duration::from_millis(1),
            usage: None,
            metadata: Default::default(),
        })
    }
}

struct RateLimitedProvider;

#[async_trait]
impl ModelProvider for RateLimitedProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: "rate_limited".to_string(),
            name: "RateLimited".to_string(),
            models: vec!["rate-limited-model".to_string()],
            max_tokens: None,
            supports_tools: false,
            capabilities: vec![],
        }
    }

    async fn generate(
        &self,
        _request: GenerationRequest,
    ) -> Result<GenerationResult, destilation_core::provider::ProviderError> {
        Err(destilation_core::provider::ProviderError::RateLimited)
    }
}

struct CriticalErrorProvider;

#[async_trait]
impl ModelProvider for CriticalErrorProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: "critical".to_string(),
            name: "Critical".to_string(),
            models: vec!["critical-model".to_string()],
            max_tokens: None,
            supports_tools: false,
            capabilities: vec![],
        }
    }

    async fn generate(
        &self,
        _request: GenerationRequest,
    ) -> Result<GenerationResult, destilation_core::provider::ProviderError> {
        Err(destilation_core::provider::ProviderError::Critical("Critical provider error".to_string()))
    }
}

fn mk_template() -> TemplateConfig {
    TemplateConfig {
        id: "simple_qa".to_string(),
        name: "Simple Q&A".to_string(),
        description: "Short question-answer pairs".to_string(),
        mode: TemplateMode::Simple,
        schema: TemplateSchema {
            version: "v1".to_string(),
            fields: vec![
                TemplateSchemaField {
                    name: "question".to_string(),
                    field_type: "string".to_string(),
                    required: true,
                },
                TemplateSchemaField {
                    name: "answer".to_string(),
                    field_type: "string".to_string(),
                    required: true,
                },
            ],
            json_schema: None,
        },
        system_prompt: "".to_string(),
        user_prompt_pattern: "".to_string(),
        examples: vec![],
        validators: vec!["structural".to_string()],
    }
}

#[tokio::test]
async fn run_job_persists_target_samples_and_completes_job() {
    let job_store: Arc<dyn JobStore> = Arc::new(InMemoryJobStore::new());
    let task_store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let metrics: Arc<dyn Metrics> = Arc::new(InMemoryMetrics::new());

    let persisted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let dataset_writer: Arc<dyn DatasetWriter> = Arc::new(InMemoryDatasetWriter {
        persisted: persisted.clone(),
    });

    let mut providers_map: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    providers_map.insert("mock".to_string(), Arc::new(MockProvider));

    let provider_configs = vec![ProviderConfig::Script {
        id: "mock".to_string(),
        name: Some("Mock".to_string()),
        enabled: true,
        command: "mock".to_string(),
        args: vec![],
        timeout_ms: None,
    }];

    let mut templates = HashMap::new();
    templates.insert("simple_qa".to_string(), mk_template());

    let validators: Vec<Arc<dyn Validator>> = vec![Arc::new(StructuralValidator::new())];

    let orchestrator = Orchestrator {
        job_store: job_store.clone(),
        task_store: task_store.clone(),
        provider_store: Arc::new(DummyStore),
        providers: Arc::new(RwLock::new(providers_map)),
        provider_configs: Arc::new(RwLock::new(provider_configs)),
        template_store: Arc::new(DummyStore),
        templates,
        validators,
        dataset_writer,
        metrics: metrics.clone(),
        logger: Arc::new(NoopEventLogger),
    };

    let job_config = JobConfig {
        id: "job-1".to_string(),
        name: "Job".to_string(),
        description: None,
        target_samples: 3,
        max_concurrency: 1,
        domains: vec![],
        template_id: "simple_qa".to_string(),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![JobProviderSpec {
            provider_id: "mock".to_string(),
            weight: 1.0,
            capabilities_required: vec![],
        }],
        validation: JobValidationConfig {
            max_attempts: 2,
            validators: vec!["structural".to_string()],
            min_quality_score: None,
            fail_fast: false,
        },
        output: JobOutputConfig {
            dataset_dir: "datasets/test".to_string(),
            shard_size: 1,
            compress: false,
            metadata: Default::default(),
        },
    };

    let job = orchestrator.submit_job(job_config).await.unwrap();
    orchestrator.run_job(&job.id).await.unwrap();

    let job_after = job_store.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(
        job_after.status,
        destilation_core::domain::JobStatus::Completed
    );
    assert_eq!(job_after.completed_samples, 3);

    let samples_len = { persisted.lock().unwrap().len() };
    assert_eq!(samples_len, 3);

    let tasks = task_store.list_tasks(&job.id).await.unwrap();
    assert_eq!(tasks.len(), 3);
    assert!(tasks
        .iter()
        .all(|t| t.state == destilation_core::domain::TaskState::Persisted));
}

#[tokio::test]
async fn run_job_writes_per_job_event_log_file() {
    let job_store: Arc<dyn JobStore> = Arc::new(InMemoryJobStore::new());
    let task_store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let metrics: Arc<dyn Metrics> = Arc::new(InMemoryMetrics::new());

    let dataset_root = std::env::temp_dir().join(format!(
        "destilation-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    ));
    let dataset_root_str = dataset_root.to_string_lossy().to_string();

    let dataset_writer: Arc<dyn DatasetWriter> =
        Arc::new(FilesystemDatasetWriter::new(dataset_root_str.clone()));

    let mut providers_map: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    providers_map.insert("mock".to_string(), Arc::new(MockProvider));

    let provider_configs = vec![ProviderConfig::Script {
        id: "mock".to_string(),
        name: Some("Mock".to_string()),
        enabled: true,
        command: "mock".to_string(),
        args: vec![],
        timeout_ms: None,
    }];

    let mut templates = HashMap::new();
    templates.insert("simple_qa".to_string(), mk_template());

    let validators: Vec<Arc<dyn Validator>> = vec![Arc::new(StructuralValidator::new())];

    let event_logger = Arc::new(BufferedFileEventLogger::new(500, 200));

    let orchestrator = Orchestrator {
        job_store: job_store.clone(),
        task_store: task_store.clone(),
        provider_store: Arc::new(DummyStore),
        providers: Arc::new(RwLock::new(providers_map)),
        provider_configs: Arc::new(RwLock::new(provider_configs)),
        template_store: Arc::new(DummyStore),
        templates,
        validators,
        dataset_writer,
        metrics: metrics.clone(),
        logger: event_logger,
    };

    let job_id = "job-log-1".to_string();
    let dataset_dir = dataset_root.join(&job_id).to_string_lossy().to_string();

    let job_config = JobConfig {
        id: job_id.clone(),
        name: "Job".to_string(),
        description: None,
        target_samples: 1,
        max_concurrency: 1,
        domains: vec![],
        template_id: "simple_qa".to_string(),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![JobProviderSpec {
            provider_id: "mock".to_string(),
            weight: 1.0,
            capabilities_required: vec![],
        }],
        validation: JobValidationConfig {
            max_attempts: 1,
            validators: vec!["structural".to_string()],
            min_quality_score: None,
            fail_fast: false,
        },
        output: JobOutputConfig {
            dataset_dir: dataset_dir.clone(),
            shard_size: 1,
            compress: false,
            metadata: Default::default(),
        },
    };

    let job = orchestrator.submit_job(job_config).await.unwrap();
    orchestrator.run_job(&job.id).await.unwrap();

    let events_path = std::path::Path::new(&dataset_dir).join(format!("{job_id}.events.jsonl"));
    let contents = std::fs::read_to_string(&events_path).unwrap();
    assert!(contents.contains("\"message\":\"job.started\""));
    assert!(contents.contains("\"message\":\"job.finished\""));

    let _ = std::fs::remove_dir_all(dataset_root);
}

#[tokio::test]
async fn run_job_with_rate_limited_provider_rejects_task() {
    let job_store: Arc<dyn JobStore> = Arc::new(InMemoryJobStore::new());
    let task_store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let metrics: Arc<dyn Metrics> = Arc::new(InMemoryMetrics::new());

    let persisted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let dataset_writer: Arc<dyn DatasetWriter> = Arc::new(InMemoryDatasetWriter {
        persisted: persisted.clone(),
    });

    let mut providers_map: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    providers_map.insert("rate_limited".to_string(), Arc::new(RateLimitedProvider));

    let provider_configs = vec![ProviderConfig::Script {
        id: "rate_limited".to_string(),
        name: Some("RateLimited".to_string()),
        enabled: true,
        command: "rate_limited".to_string(),
        args: vec![],
        timeout_ms: None,
    }];

    let mut templates = HashMap::new();
    templates.insert("simple_qa".to_string(), mk_template());

    let validators: Vec<Arc<dyn Validator>> = vec![Arc::new(StructuralValidator::new())];

    let orchestrator = Orchestrator {
        job_store: job_store.clone(),
        task_store: task_store.clone(),
        provider_store: Arc::new(DummyStore),
        providers: Arc::new(RwLock::new(providers_map)),
        provider_configs: Arc::new(RwLock::new(provider_configs)),
        template_store: Arc::new(DummyStore),
        templates,
        validators,
        dataset_writer,
        metrics: metrics.clone(),
        logger: Arc::new(NoopEventLogger),
    };

    let job_config = JobConfig {
        id: "job-rate-limit".to_string(),
        name: "Job".to_string(),
        description: None,
        target_samples: 1,
        max_concurrency: 1,
        domains: vec![],
        template_id: "simple_qa".to_string(),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![JobProviderSpec {
            provider_id: "rate_limited".to_string(),
            weight: 1.0,
            capabilities_required: vec![],
        }],
        validation: JobValidationConfig {
            max_attempts: 2,
            validators: vec!["structural".to_string()],
            min_quality_score: None,
            fail_fast: false,
        },
        output: JobOutputConfig {
            dataset_dir: "datasets/test".to_string(),
            shard_size: 1,
            compress: false,
            metadata: Default::default(),
        },
    };

    let job = orchestrator.submit_job(job_config).await.unwrap();
    orchestrator.run_job(&job.id).await.unwrap();

    let job_after = job_store.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(
        job_after.status,
        destilation_core::domain::JobStatus::Failed
    );
    assert_eq!(job_after.completed_samples, 0);

    let samples_len = { persisted.lock().unwrap().len() };
    assert_eq!(samples_len, 0);

    let tasks = task_store.list_tasks(&job.id).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, destilation_core::domain::TaskState::Rejected);
}

#[tokio::test]
async fn run_job_with_critical_error_provider_pauses_job() {
    let job_store: Arc<dyn JobStore> = Arc::new(InMemoryJobStore::new());
    let task_store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let metrics: Arc<dyn Metrics> = Arc::new(InMemoryMetrics::new());

    let persisted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let dataset_writer: Arc<dyn DatasetWriter> = Arc::new(InMemoryDatasetWriter {
        persisted: persisted.clone(),
    });

    let mut providers_map: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    providers_map.insert("critical".to_string(), Arc::new(CriticalErrorProvider));

    let provider_configs = vec![ProviderConfig::Script {
        id: "critical".to_string(),
        name: Some("Critical".to_string()),
        enabled: true,
        command: "critical".to_string(),
        args: vec![],
        timeout_ms: None,
    }];

    let mut templates = HashMap::new();
    templates.insert("simple_qa".to_string(), mk_template());

    let validators: Vec<Arc<dyn Validator>> = vec![Arc::new(StructuralValidator::new())];

    let orchestrator = Orchestrator {
        job_store: job_store.clone(),
        task_store: task_store.clone(),
        provider_store: Arc::new(DummyStore),
        providers: Arc::new(RwLock::new(providers_map)),
        provider_configs: Arc::new(RwLock::new(provider_configs)),
        template_store: Arc::new(DummyStore),
        templates,
        validators,
        dataset_writer,
        metrics: metrics.clone(),
        logger: Arc::new(NoopEventLogger),
    };

    let job_config = JobConfig {
        id: "job-critical".to_string(),
        name: "Job".to_string(),
        description: None,
        target_samples: 1,
        max_concurrency: 1,
        domains: vec![],
        template_id: "simple_qa".to_string(),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![JobProviderSpec {
            provider_id: "critical".to_string(),
            weight: 1.0,
            capabilities_required: vec![],
        }],
        validation: JobValidationConfig {
            max_attempts: 2,
            validators: vec!["structural".to_string()],
            min_quality_score: None,
            fail_fast: false,
        },
        output: JobOutputConfig {
            dataset_dir: "datasets/test".to_string(),
            shard_size: 1,
            compress: false,
            metadata: Default::default(),
        },
    };

    let job = orchestrator.submit_job(job_config).await.unwrap();
    let result = orchestrator.run_job(&job.id).await;
    assert!(result.is_err());

    let job_after = job_store.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(
        job_after.status,
        destilation_core::domain::JobStatus::Paused
    );
    assert_eq!(job_after.completed_samples, 0);

    let samples_len = { persisted.lock().unwrap().len() };
    assert_eq!(samples_len, 0);

    let tasks = task_store.list_tasks(&job.id).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, destilation_core::domain::TaskState::Queued);
}

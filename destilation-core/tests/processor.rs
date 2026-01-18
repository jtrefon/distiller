use destilation_core::domain::*;
use destilation_core::metrics::{InMemoryMetrics, Metrics};
use destilation_core::processor::*;
use destilation_core::provider::*;
use destilation_core::queue::*;
use destilation_core::storage::*;
use destilation_core::validation::Validator;
use destilation_core::validators::{StructuralValidator, DedupValidator};
use std::sync::Arc;
use tokio::sync::RwLock;

struct MockProvider;

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: "mock".to_string(),
            name: "Mock Provider".to_string(),
            models: vec!["mock-model".to_string()],
            max_tokens: None,
            supports_tools: false,
            capabilities: vec![],
        }
    }

    async fn generate(&self, _request: GenerationRequest) -> Result<GenerationResult, ProviderError> {
        Ok(GenerationResult {
            provider_id: "mock".to_string(),
            model: "mock-model".to_string(),
            raw_output: serde_json::json!({"question": "What is Rust?", "answer": "A programming language"}).to_string(),
            latency: std::time::Duration::from_millis(100),
            usage: None,
            metadata: Default::default(),
        })
    }
}

struct DummyProviderStore;

#[async_trait::async_trait]
impl destilation_core::storage::ProviderStore for DummyProviderStore {
    fn save_provider(&self, _provider: &ProviderConfig) -> anyhow::Result<()> {
        Ok(())
    }

    fn delete_provider(&self, _id: &ProviderId) -> anyhow::Result<()> {
        Ok(())
    }

    fn list_providers(&self) -> anyhow::Result<Vec<ProviderConfig>> {
        Ok(vec![])
    }
}

struct DummyTemplateStore;

#[async_trait::async_trait]
impl destilation_core::storage::TemplateStore for DummyTemplateStore {
    fn save_template(&self, _template: &TemplateConfig) -> anyhow::Result<()> {
        Ok(())
    }

    fn delete_template(&self, _id: &TemplateId) -> anyhow::Result<()> {
        Ok(())
    }

    fn get_template(&self, _id: &TemplateId) -> anyhow::Result<Option<TemplateConfig>> {
        Ok(None)
    }

    fn list_templates(&self) -> anyhow::Result<Vec<TemplateConfig>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_processor_task_processing() {
    // Setup providers
    let mut providers_map = std::collections::HashMap::new();
    providers_map.insert("mock".to_string(), Arc::new(MockProvider) as Arc<dyn ModelProvider>);
    let providers = Arc::new(RwLock::new(providers_map));

    // Setup validators
    let validators = vec![
        Arc::new(StructuralValidator::new()) as Arc<dyn Validator>,
        Arc::new(DedupValidator::new()) as Arc<dyn Validator>,
    ];

    // Setup storage
    let task_store = Arc::new(InMemoryTaskStore::new());
    let job_store = Arc::new(InMemoryJobStore::new());
    let provider_store = Arc::new(DummyProviderStore);
    let template_store = Arc::new(DummyTemplateStore);
    let dataset_writer = Arc::new(FilesystemDatasetWriter::new("datasets/test_processor".to_string()));

    // Create job
    let job_config = JobConfig {
        id: "test-job-1".to_string(),
        name: "Test Job".to_string(),
        description: None,
        target_samples: 1,
        max_concurrency: 1,
        domains: vec![],
        template_id: "simple_qa".to_string(),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![],
        validation: JobValidationConfig {
            max_attempts: 1,
            validators: vec!["structural".to_string()],
            min_quality_score: None,
            fail_fast: false,
        },
        output: JobOutputConfig {
            dataset_dir: "datasets/test_processor".to_string(),
            shard_size: 1000,
            compress: false,
            metadata: Default::default(),
        },
    };

    let job = job_store.create_job(job_config).await.unwrap();

    // Create test task
    let task = Task {
        id: "test-task-1".to_string(),
        job_id: job.id.clone(),
        state: TaskState::Queued,
        attempts: 0,
        max_attempts: 1,
        provider_id: Some("mock".to_string()),
        domain_id: "general".to_string(),
        template_id: "simple_qa".to_string(),
        prompt_spec: PromptSpec {
            system: Some("You are a helpful assistant".to_string()),
            user: "Answer the question".to_string(),
            extra: Default::default(),
        },
        raw_response: None,
        validation_result: None,
        quality_score: None,
        is_negative: false,
    };

    task_store.enqueue_task(task).await.unwrap();

    // Create processor
    let metrics = Arc::new(InMemoryMetrics::new());
    let logger = Arc::new(destilation_core::logging::NoopEventLogger);
    let processor = Arc::new(StandardProcessor::new(
        providers,
        validators,
        dataset_writer,
        task_store.clone(),
        job_store,
        metrics.clone(),
        logger,
        None,
    ));

    // Process the task
    let tasks = task_store.list_tasks(&job.id).await.unwrap();
    assert!(!tasks.is_empty());
    let task = tasks.first().unwrap().clone();

    let result = processor.process_task(task.clone()).await;
    assert!(result.is_ok());

    // Verify task is processed
    let tasks = task_store.list_tasks(&job.id).await.unwrap();
    let processed_task = tasks.first().unwrap();
    assert_eq!(processed_task.state, TaskState::Persisted);
    assert!(processed_task.raw_response.is_some());

    // Verify metrics
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.samples_persisted, 1);
}

#[tokio::test]
async fn test_processor_with_queue_and_workers() {
    // Setup providers
    let mut providers_map = std::collections::HashMap::new();
    providers_map.insert("mock".to_string(), Arc::new(MockProvider) as Arc<dyn ModelProvider>);
    let providers = Arc::new(RwLock::new(providers_map));

    // Setup validators
    let validators = vec![Arc::new(StructuralValidator::new()) as Arc<dyn Validator>];

    // Setup storage
    let task_store = Arc::new(InMemoryTaskStore::new());
    let job_store = Arc::new(InMemoryJobStore::new());
    let provider_store = Arc::new(DummyProviderStore);
    let template_store = Arc::new(DummyTemplateStore);
    let dataset_writer = Arc::new(FilesystemDatasetWriter::new("datasets/test_processor_2".to_string()));

    // Create job
    let job_config = JobConfig {
        id: "test-job-2".to_string(),
        name: "Test Job".to_string(),
        description: None,
        target_samples: 5,
        max_concurrency: 1,
        domains: vec![],
        template_id: "simple_qa".to_string(),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![],
        validation: JobValidationConfig {
            max_attempts: 1,
            validators: vec!["structural".to_string()],
            min_quality_score: None,
            fail_fast: false,
        },
        output: JobOutputConfig {
            dataset_dir: "datasets/test_processor_2".to_string(),
            shard_size: 1000,
            compress: false,
            metadata: Default::default(),
        },
    };

    let job = job_store.create_job(job_config).await.unwrap();

    // Create tasks
    let mut tasks = Vec::new();
    for i in 0..5 {
        let task = Task {
            id: format!("test-task-{}", i),
            job_id: job.id.clone(),
            state: TaskState::Queued,
            attempts: 0,
            max_attempts: 1,
            provider_id: Some("mock".to_string()),
            domain_id: "general".to_string(),
            template_id: "simple_qa".to_string(),
            prompt_spec: PromptSpec {
                system: Some("You are a helpful assistant".to_string()),
                user: "Answer the question".to_string(),
                extra: Default::default(),
            },
            raw_response: None,
            validation_result: None,
            quality_score: None,
            is_negative: false,
        };
        task_store.enqueue_task(task.clone()).await.unwrap();
        tasks.push(task);
    }

    // Create queue and add tasks
    let logger = Arc::new(destilation_core::logging::NoopEventLogger);
    let queue = Arc::new(MemoryTaskQueue::new(None, logger));
    for task in tasks.iter() {
        queue.enqueue(task.clone()).await.unwrap();
    }

    // Create processor and manager
    let metrics = Arc::new(InMemoryMetrics::new());
    let logger = Arc::new(destilation_core::logging::NoopEventLogger);
    let processor = Arc::new(StandardProcessor::new(
        providers,
        validators,
        dataset_writer,
        task_store.clone(),
        job_store,
        metrics.clone(),
        logger.clone(),
        None,
    ));

    let mut manager = ProcessorManager::new();
    manager.start(2, processor, queue, logger).await;

    // Wait for tasks to be processed
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Shutdown manager
    manager.stop().await;

    // Verify all tasks are processed
    let processed_tasks = task_store.list_tasks(&job.id).await.unwrap();
    assert_eq!(processed_tasks.len(), 5);
    for task in processed_tasks {
        assert_eq!(task.state, TaskState::Persisted);
        assert!(task.raw_response.is_some());
    }

    // Verify metrics
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.samples_persisted, 5);
}

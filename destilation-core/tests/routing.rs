use destilation_core::domain::{
    DomainSpec, JobConfig, JobOutputConfig, JobProviderSpec, JobValidationConfig, ReasoningMode,
};
use destilation_core::metrics::InMemoryMetrics;
use destilation_core::orchestrator::Orchestrator;
use destilation_core::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderMetadata,
};
use destilation_core::storage::{DatasetWriter, JobStore, TaskStore};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

struct DummyStore;
#[async_trait::async_trait]
impl JobStore for DummyStore {
    async fn create_job(
        &self,
        _config: destilation_core::domain::JobConfig,
    ) -> anyhow::Result<destilation_core::domain::Job> {
        unimplemented!()
    }
    async fn get_job(&self, _id: &String) -> anyhow::Result<Option<destilation_core::domain::Job>> {
        unimplemented!()
    }
    async fn update_job(&self, _job: &destilation_core::domain::Job) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn list_jobs(&self) -> anyhow::Result<Vec<destilation_core::domain::Job>> {
        unimplemented!()
    }
    async fn delete_job(&self, _id: &String) -> anyhow::Result<()> {
        unimplemented!()
    }
}
#[async_trait::async_trait]
impl TaskStore for DummyStore {
    async fn enqueue_task(&self, _task: destilation_core::domain::Task) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn fetch_next_task(&self) -> anyhow::Result<Option<destilation_core::domain::Task>> {
        unimplemented!()
    }
    async fn update_task(&self, _task: &destilation_core::domain::Task) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn list_tasks(
        &self,
        _job_id: &String,
    ) -> anyhow::Result<Vec<destilation_core::domain::Task>> {
        unimplemented!()
    }
    async fn delete_tasks_by_job(&self, _job_id: &String) -> anyhow::Result<()> {
        unimplemented!()
    }
}
#[async_trait::async_trait]
impl DatasetWriter for DummyStore {
    async fn persist_sample(
        &self,
        _sample: destilation_core::domain::Sample,
    ) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn flush(&self) -> anyhow::Result<()> {
        unimplemented!()
    }
}

struct StaticProvider {
    id: String,
    capabilities: Vec<String>,
}
#[async_trait::async_trait]
impl ModelProvider for StaticProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: self.id.clone(),
            name: self.id.clone(),
            models: vec!["m".to_string()],
            max_tokens: None,
            supports_tools: false,
            capabilities: self.capabilities.clone(),
        }
    }
    async fn generate(
        &self,
        _request: GenerationRequest,
    ) -> Result<GenerationResult, destilation_core::provider::ProviderError> {
        unimplemented!()
    }
}

#[tokio::test]
async fn select_provider_respects_capabilities() {
    let mut providers: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    providers.insert(
        "p1".to_string(),
        Arc::new(StaticProvider {
            id: "p1".to_string(),
            capabilities: vec!["general".to_string()],
        }),
    );
    providers.insert(
        "p2".to_string(),
        Arc::new(StaticProvider {
            id: "p2".to_string(),
            capabilities: vec!["reasoning".to_string()],
        }),
    );

    let provider_configs = Arc::new(RwLock::new(Vec::new()));
    {
        let mut configs = provider_configs.write().await;
        // Add enabled configs for p1 and p2
        configs.push(destilation_core::provider::ProviderConfig::Script {
            id: "p1".to_string(),
            name: Some("p1".to_string()),
            enabled: true,
            command: "echo".to_string(),
            args: vec![],
            timeout_ms: Some(1000),
        });
        configs.push(destilation_core::provider::ProviderConfig::Script {
            id: "p2".to_string(),
            name: Some("p2".to_string()),
            enabled: true,
            command: "echo".to_string(),
            args: vec![],
            timeout_ms: Some(1000),
        });
    }

    let orch = Orchestrator {
        job_store: Arc::new(DummyStore),
        task_store: Arc::new(DummyStore),
        providers: Arc::new(RwLock::new(providers)),
        provider_configs,
        templates: HashMap::new(),
        validators: Vec::new(),
        dataset_writer: Arc::new(DummyStore),
        metrics: Arc::new(InMemoryMetrics::new()),
    };
    let job = JobConfig {
        id: "job".to_string(),
        name: "Job".to_string(),
        description: None,
        target_samples: 1,
        max_concurrency: 1,
        domains: vec![DomainSpec {
            id: "d".to_string(),
            name: "D".to_string(),
            weight: 1.0,
            tags: vec![],
        }],
        template_id: "t".to_string(),
        reasoning_mode: ReasoningMode::Reasoning,
        providers: vec![
            JobProviderSpec {
                provider_id: "p1".to_string(),
                weight: 1.0,
                capabilities_required: vec!["reasoning".to_string()],
            },
            JobProviderSpec {
                provider_id: "p2".to_string(),
                weight: 1.0,
                capabilities_required: vec!["reasoning".to_string()],
            },
        ],
        validation: JobValidationConfig {
            max_attempts: 1,
            validators: vec![],
            min_quality_score: None,
            fail_fast: true,
        },
        output: JobOutputConfig {
            dataset_dir: "datasets/test".to_string(),
            shard_size: 1,
            compress: false,
            metadata: Default::default(),
        },
    };
    let sel = orch
        .select_provider_id(&job)
        .await
        .expect("provider selected");
    assert_eq!(sel, "p2");
}

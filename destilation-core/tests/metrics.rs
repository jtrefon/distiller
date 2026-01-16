use destilation_core::domain::{
    DomainSpec, JobConfig, JobOutputConfig, JobProviderSpec, JobValidationConfig, ReasoningMode,
    TemplateConfig, TemplateId, TemplateMode, TemplateSchema, TemplateSchemaField,
};
use destilation_core::metrics::{InMemoryMetrics, Metrics};
use destilation_core::orchestrator::Orchestrator;
use destilation_core::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderMetadata,
};
use destilation_core::storage::{FilesystemDatasetWriter, InMemoryJobStore, InMemoryTaskStore};
use destilation_core::validation::Validator;
use destilation_core::validators::StructuralValidator;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

struct TestProvider;

#[async_trait::async_trait]
impl ModelProvider for TestProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: "mock".to_string(),
            name: "TestProvider".to_string(),
            models: vec!["mock".to_string()],
            max_tokens: None,
            supports_tools: false,
            capabilities: vec!["general".to_string()],
        }
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResult, destilation_core::provider::ProviderError> {
        Ok(GenerationResult {
            provider_id: request.provider_id,
            model: request.model,
            raw_output: "{\"question\": \"q\", \"answer\": \"a\"}".to_string(),
            latency: std::time::Duration::from_millis(10),
            usage: None,
            metadata: HashMap::new(),
        })
    }
}

#[tokio::test]
async fn metrics_count_basic_flow() {
    let job_store = Arc::new(InMemoryJobStore::new());
    let task_store = Arc::new(InMemoryTaskStore::new());
    let dataset_writer = Arc::new(FilesystemDatasetWriter::new(
        "datasets/test_metrics".to_string(),
    ));

    let mut providers: HashMap<String, Arc<dyn destilation_core::provider::ModelProvider>> =
        HashMap::new();
    providers.insert(
        "mock".to_string(),
        Arc::new(TestProvider),
    );

    let template = TemplateConfig {
        id: TemplateId::from("simple_qa"),
        name: "Simple QA".to_string(),
        description: "Q/A template".to_string(),
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
        system_prompt: "You are helpful.".to_string(),
        user_prompt_pattern: "Answer the question.".to_string(),
        examples: vec![],
        validators: vec!["structural".to_string()],
    };
    let mut templates = HashMap::new();
    templates.insert(template.id.clone(), template);

    let validators: Vec<Arc<dyn Validator>> = vec![Arc::new(StructuralValidator::new())];

    let metrics = Arc::new(InMemoryMetrics::new());

    let provider_configs = Arc::new(RwLock::new(Vec::new()));
    // Mock config for the mock provider so orchestrator can find model name
    {
        let mut configs = provider_configs.write().await;
        configs.push(destilation_core::provider::ProviderConfig::OpenRouter { 
            id: "mock".to_string(),
            name: Some("TestProvider".to_string()),
            enabled: true,
            base_url: "http://localhost".to_string(),
            api_key: "key".to_string(),
            model: "mock".to_string(),
        });
    }

    let orch = Orchestrator {
        job_store,
        task_store,
        providers: Arc::new(RwLock::new(providers)),
        provider_configs,
        templates,
        validators,
        dataset_writer,
        metrics: metrics.clone(),
    };

    let job_config = JobConfig {
        id: "job-metrics-001".to_string(),
        name: "Metrics Job".to_string(),
        description: None,
        target_samples: 1,
        max_concurrency: 1,
        domains: vec![DomainSpec {
            id: "d".to_string(),
            name: "D".to_string(),
            weight: 1.0,
            tags: vec![],
        }],
        template_id: TemplateId::from("simple_qa"),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![JobProviderSpec {
            provider_id: "mock".to_string(),
            weight: 1.0,
            capabilities_required: vec!["general".to_string()],
        }],
        validation: JobValidationConfig {
            max_attempts: 1,
            validators: vec!["structural".to_string()],
            min_quality_score: None,
            fail_fast: true,
        },
        output: JobOutputConfig {
            dataset_dir: "datasets/test_metrics".to_string(),
            shard_size: 1,
            compress: false,
            metadata: Default::default(),
        },
    };

    let job = orch.submit_job(job_config).await.expect("submit ok");
    orch.run_job(&job.id).await.expect("run ok");

    let snap = metrics.snapshot();
    assert_eq!(snap.jobs_submitted, 1);
    assert!(snap.tasks_enqueued >= 1);
    assert!(snap.tasks_started >= 1);
    assert!(snap.tasks_persisted >= 1);
    assert_eq!(snap.tasks_rejected, 0);
    assert_eq!(snap.samples_persisted, 1);
    assert!(snap.validator_pass >= 1);
    assert_eq!(snap.validator_fail, 0);
}

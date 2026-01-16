use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type JobId = String;
pub type TaskId = String;
pub type SampleId = String;
pub type ProviderId = String;
pub type TemplateId = String;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Generating,
    Validating,
    Persisted,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobConfig {
    pub id: JobId,
    pub name: String,
    pub description: Option<String>,
    pub target_samples: u64,
    pub max_concurrency: u32,
    pub domains: Vec<DomainSpec>,
    pub template_id: TemplateId,
    pub reasoning_mode: ReasoningMode,
    pub providers: Vec<JobProviderSpec>,
    pub validation: JobValidationConfig,
    pub output: JobOutputConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainSpec {
    pub id: String,
    pub name: String,
    pub weight: f32,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReasoningMode {
    Simple,
    Moe,
    Reasoning,
    Tools,
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobProviderSpec {
    pub provider_id: ProviderId,
    pub weight: f32,
    pub capabilities_required: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobValidationConfig {
    pub max_attempts: u32,
    pub validators: Vec<String>,
    pub min_quality_score: Option<f32>,
    pub fail_fast: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobOutputConfig {
    pub dataset_dir: String,
    pub shard_size: u64,
    pub compress: bool,
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub config: JobConfig,
    pub status: JobStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub completed_samples: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub job_id: JobId,
    pub state: TaskState,
    pub attempts: u32,
    pub max_attempts: u32,
    pub provider_id: Option<ProviderId>,
    pub domain_id: String,
    pub template_id: TemplateId,
    pub prompt_spec: PromptSpec,
    pub raw_response: Option<String>,
    pub validation_result: Option<crate::validation::ValidationOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub id: SampleId,
    pub job_id: JobId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub model_name: String,
    pub template_id: TemplateId,
    pub schema_version: String,
    pub payload: serde_json::Value,
    pub quality_score: Option<f32>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplateSchemaField {
    pub name: String,
    pub field_type: String,
    pub required: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplateSchema {
    pub version: String,
    pub fields: Vec<TemplateSchemaField>,
    pub json_schema: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplateConfig {
    pub id: TemplateId,
    pub name: String,
    pub description: String,
    pub mode: TemplateMode,
    pub schema: TemplateSchema,
    pub system_prompt: String,
    pub user_prompt_pattern: String,
    pub examples: Vec<TemplateExample>,
    pub validators: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TemplateMode {
    Simple,
    Moe,
    Reasoning,
    Tools,
    Custom,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplateExample {
    pub input: serde_json::Value,
    pub output: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptSpec {
    pub system: Option<String>,
    pub user: String,
    pub extra: HashMap<String, String>,
}

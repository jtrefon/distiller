use crate::domain::{Job, Task, TemplateConfig};
use crate::provider::GenerationResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationContext {
    pub job: Job,
    pub task: Task,
    pub template: TemplateConfig,
    pub provider_result: GenerationResult,
    pub parsed_output: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    pub severity: ValidationSeverity,
    pub details: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ValidationSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationOutcome {
    pub passed: bool,
    pub issues: Vec<ValidationIssue>,
    pub score: Option<f32>,
}

pub trait Validator: Send + Sync {
    fn id(&self) -> &str;
    fn validate(&self, ctx: &ValidationContext) -> ValidationOutcome;
}

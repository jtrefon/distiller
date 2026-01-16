use crate::domain::{PromptSpec, ProviderId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub id: ProviderId,
    pub name: String,
    pub models: Vec<String>,
    pub max_tokens: Option<u32>,
    pub supports_tools: bool,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationRequest {
    pub provider_id: ProviderId,
    pub model: String,
    pub prompt: PromptSpec,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationResult {
    pub provider_id: ProviderId,
    pub model: String,
    pub raw_output: String,
    pub latency: Duration,
    pub usage: Option<TokenUsage>,
    pub metadata: HashMap<String, String>,
}

#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("transport error")]
    Transport,
    #[error("rate limited")]
    RateLimited,
    #[error("invalid response")]
    InvalidResponse,
    #[error("provider unavailable")]
    Unavailable,
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;

    async fn generate(&self, request: GenerationRequest)
        -> Result<GenerationResult, ProviderError>;
}

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
    #[error("request timed out")]
    Timeout,
    #[error("rate limited")]
    RateLimited,
    #[error("invalid response")]
    InvalidResponse,
    #[error("provider unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProviderConfig {
    OpenRouter {
        id: ProviderId,
        name: Option<String>,
        enabled: bool,
        base_url: String,
        api_key: String,
        model: String,
    },
    Ollama {
        id: ProviderId,
        name: Option<String>,
        enabled: bool,
        base_url: String,
        model: String,
    },
    Script {
        id: ProviderId,
        name: Option<String>,
        enabled: bool,
        command: String,
        args: Vec<String>,
        timeout_ms: Option<u64>,
    },
}

impl ProviderConfig {
    pub fn id(&self) -> &ProviderId {
        match self {
            Self::OpenRouter { id, .. } => id,
            Self::Ollama { id, .. } => id,
            Self::Script { id, .. } => id,
        }
    }

    pub fn name(&self) -> Option<&String> {
        match self {
            Self::OpenRouter { name, .. } => name.as_ref(),
            Self::Ollama { name, .. } => name.as_ref(),
            Self::Script { name, .. } => name.as_ref(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        match self {
            Self::OpenRouter { enabled, .. } => *enabled,
            Self::Ollama { enabled, .. } => *enabled,
            Self::Script { enabled, .. } => *enabled,
        }
    }

    pub fn set_enabled(&mut self, is_enabled: bool) {
        match self {
            Self::OpenRouter { enabled, .. } => *enabled = is_enabled,
            Self::Ollama { enabled, .. } => *enabled = is_enabled,
            Self::Script { enabled, .. } => *enabled = is_enabled,
        }
    }
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;

    async fn generate(&self, request: GenerationRequest)
        -> Result<GenerationResult, ProviderError>;

    async fn health_check(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}

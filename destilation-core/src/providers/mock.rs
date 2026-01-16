use crate::domain::ProviderId;
use crate::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderError, ProviderMetadata,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

pub struct MockProvider {
    id: ProviderId,
    name: String,
}

impl MockProvider {
    pub fn new(id: ProviderId) -> Self {
        Self {
            id,
            name: "MockProvider".to_string(),
        }
    }
}

#[async_trait]
impl ModelProvider for MockProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            models: vec!["mock".to_string()],
            max_tokens: Some(2048),
            supports_tools: false,
            capabilities: vec!["general".to_string()],
        }
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResult, ProviderError> {
        let output = serde_json::json!({
            "question": "What is a binary search?",
            "answer": "Binary search is an efficient algorithm to find an item in a sorted list by repeatedly dividing the search interval in half."
        })
        .to_string();

        Ok(GenerationResult {
            provider_id: request.provider_id,
            model: "mock".to_string(),
            raw_output: output,
            latency: Duration::from_millis(5),
            usage: None,
            metadata: HashMap::new(),
        })
    }
}

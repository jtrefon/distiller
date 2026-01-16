use crate::domain::ProviderId;
use crate::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderError, ProviderMetadata,
};
use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

pub struct OllamaProvider {
    id: ProviderId,
    name: String,
    client: Client,
    base_url: String,
    model: String,
}

impl OllamaProvider {
    pub fn new(id: ProviderId, base_url: String, model: String) -> Self {
        let client = reqwest::ClientBuilder::new()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            id,
            name: "OllamaProvider".to_string(),
            client,
            base_url,
            model,
        }
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            models: vec![self.model.clone()],
            max_tokens: None,
            supports_tools: false,
            capabilities: vec!["reasoning".to_string(), "local".to_string()],
        }
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResult, ProviderError> {
        let url = format!("{}/api/generate", self.base_url.trim_end_matches('/'));
        let payload = serde_json::json!({
            "model": self.model,
            "prompt": request.prompt.user,
            "system": request.prompt.system,
            "stream": false
        });

        let start = std::time::Instant::now();
        let resp = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Transport
                }
            })?;

        if !resp.status().is_success() {
            return Err(ProviderError::InvalidResponse);
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| ProviderError::InvalidResponse)?;
        let content = body.get("response").and_then(|v| v.as_str()).unwrap_or("");

        Ok(GenerationResult {
            provider_id: request.provider_id,
            model: self.model.clone(),
            raw_output: content.to_string(),
            latency: start.elapsed(),
            usage: None,
            metadata: HashMap::new(),
        })
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
        let resp = self.client.get(url).send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Transport
            }
        })?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ProviderError::Unavailable)
        }
    }
}

use crate::domain::ProviderId;
use crate::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderError, ProviderMetadata,
};
use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;

pub struct OpenRouterProvider {
    id: ProviderId,
    name: String,
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenRouterProvider {
    pub fn new(id: ProviderId, base_url: String, api_key: String, model: String) -> Self {
        Self {
            id,
            name: "OpenRouterProvider".to_string(),
            client: Client::new(),
            base_url,
            api_key,
            model,
        }
    }
}

#[async_trait]
impl ModelProvider for OpenRouterProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            models: vec![self.model.clone()],
            max_tokens: None,
            supports_tools: true,
            capabilities: vec!["reasoning".to_string(), "general".to_string()],
        }
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResult, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut messages = Vec::<serde_json::Value>::new();
        if let Some(sys) = &request.prompt.system {
            messages.push(serde_json::json!({"role": "system", "content": sys}));
        }
        messages.push(serde_json::json!({"role": "user", "content": request.prompt.user}));

        let payload = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "temperature": request.temperature.unwrap_or(0.7),
            "top_p": request.top_p.unwrap_or(1.0),
        });

        let start = std::time::Instant::now();
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|_| ProviderError::Transport)?;

        if !resp.status().is_success() {
            return Err(ProviderError::InvalidResponse);
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| ProviderError::InvalidResponse)?;
        let content = body
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok(GenerationResult {
            provider_id: request.provider_id,
            model: self.model.clone(),
            raw_output: content.to_string(),
            latency: start.elapsed(),
            usage: None,
            metadata: HashMap::new(),
        })
    }
}

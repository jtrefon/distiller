use crate::domain::ProviderId;
use crate::logging::{LogEvent, LogLevel, SharedEventLogger};
use crate::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderError, ProviderMetadata,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

#[derive(serde::Deserialize)]
pub(crate) struct OllamaTag {
    name: String,
    size: Option<u64>,
}

#[derive(serde::Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaTag>,
}

pub struct OllamaProvider {
    id: ProviderId,
    name: String,
    client: Client,
    base_url: String,
    model: String,
    logger: SharedEventLogger,
    stream_timeout: Duration,
}

impl OllamaProvider {
    /// Create a new Ollama provider with the default stream timeout (300s)
    pub fn new(id: ProviderId, base_url: String, model: String, logger: SharedEventLogger) -> Self {
        let client = reqwest::ClientBuilder::new()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(900))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            id,
            name: "OllamaProvider".to_string(),
            client,
            base_url,
            model,
            logger,
            stream_timeout: Duration::from_secs(300),
        }
    }

    pub fn with_client(id: ProviderId, base_url: String, model: String, client: Client, logger: SharedEventLogger) -> Self {
        Self {
            id,
            name: "OllamaProvider".to_string(),
            client,
            base_url,
            model,
            logger,
            stream_timeout: Duration::from_secs(300),
        }
    }

    /// Create a provider with a custom stream timeout (useful for tests)
    pub fn with_client_and_timeout(id: ProviderId, base_url: String, model: String, client: Client, logger: SharedEventLogger, stream_timeout: Duration) -> Self {
        Self {
            id,
            name: "OllamaProvider".to_string(),
            client,
            base_url,
            model,
            logger,
            stream_timeout,
        }
    }

    /// Fetch available models from Ollama server
    pub(crate) async fn list_models(&self) -> Result<Vec<OllamaTag>, ProviderError> {
        let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
        let resp = self.client.get(url).send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Transport
            }
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let msg = format!("Ollama Error {}: {}", status, body);
            return Err(ProviderError::Critical(msg));
        }

        let body: OllamaTagsResponse = resp
            .json()
            .await
            .map_err(|_| ProviderError::InvalidResponse)?;
        
        Ok(body.models)
    }

    async fn resolve_model_name(&self) -> Result<String, ProviderError> {
        match self.list_models().await {
            Ok(models) => {
                if models.is_empty() {
                    return Ok(self.model.clone());
                }
                if let Some(model) = models.iter().find(|m| m.name == self.model) {
                    return self.ensure_model_downloaded(model);
                }

                let mut candidates = Vec::new();
                if let Some(last_segment) = self.model.split('/').last() {
                    candidates.push(last_segment.to_string());
                }
                if let Some(base) = self.model.split(':').next() {
                    candidates.push(base.to_string());
                }
                if let Some(last_segment) = self.model.split('/').last() {
                    if let Some(base) = last_segment.split(':').next() {
                        candidates.push(base.to_string());
                    }
                }

                for candidate in candidates {
                    if let Some(model) = models.iter().find(|m| m.name == candidate) {
                        return self.ensure_model_downloaded(model);
                    }
                }

                Err(ProviderError::Critical(format!(
                    "Ollama model '{}' not found. Available models: {}",
                    self.model,
                    models
                        .iter()
                        .map(|m| m.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
            Err(e) => Err(e),
        }
    }

    fn ensure_model_downloaded(&self, model: &OllamaTag) -> Result<String, ProviderError> {
        let size = model.size.unwrap_or(0);
        if size == 0 {
            return Err(ProviderError::Critical(format!(
                "Ollama model '{}' is listed but not downloaded (size=0). Run 'ollama pull {}'.",
                model.name, model.name
            )));
        }
        Ok(model.name.clone())
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
        let model_name = self.resolve_model_name().await?;
        self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.model.resolved").with_field("model", model_name.clone()));
        let payload = serde_json::json!({
            "model": model_name,
            "prompt": request.prompt.user,
            "system": request.prompt.system,
            "stream": true
        });

        self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.request.send"));
        let start = std::time::Instant::now();
        let send_fut = self.client.post(url).json(&payload).send();
        let resp = match tokio::time::timeout(self.stream_timeout, send_fut).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                if e.is_timeout() {
                    return Err(ProviderError::Timeout);
                } else {
                    return Err(ProviderError::Transport);
                }
            }
            Err(_) => return Err(ProviderError::Timeout),
        };

        self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.request.sent").with_field("status", resp.status().to_string()));
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let msg = format!("Ollama Error {}: {}", status, body);
            return Err(ProviderError::Critical(msg));
        }
        self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.response.success").with_field("status", resp.status().to_string()));
        let mut stream = resp.bytes_stream();

        let streaming_result = timeout(self.stream_timeout, async {
            let mut buffer: Vec<u8> = Vec::new();
            let mut content = String::new();
            let mut done_received = false;

            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| ProviderError::Transport)?;
                self.logger.log(LogEvent::new(LogLevel::Trace, "ollama.chunk.received").with_field("size", chunk.len().to_string()));
                buffer.extend_from_slice(&chunk);

                while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
                    let line = buffer.drain(..=pos).collect::<Vec<u8>>();
                    let line = std::str::from_utf8(&line)
                        .map_err(|_| ProviderError::InvalidResponse)?
                        .trim();
                    if line.is_empty() {
                        continue;
                    }

                    let value: serde_json::Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(e) => {
                            self.logger.log(LogEvent::new(LogLevel::Warn, "ollama.json.parse.error").with_field("error", e.to_string()).with_field("line", line.to_string()));
                            continue;
                        }
                    };
                    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
                        return Err(ProviderError::Critical(err.to_string()));
                    }
                    if let Some(delta) = value.get("response") {
                        if let Some(text) = delta.as_str() {
                            content.push_str(text);
                        } else {
                            content.push_str(&delta.to_string());
                        }
                    }
                    if value.get("done").and_then(|v| v.as_bool()) == Some(true) {
                        self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.done.received"));
                        done_received = true;
                        buffer.clear();
                        break;
                    }
                }

                if buffer.is_empty() {
                    continue;
                }
            }

            self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.stream.ended").with_field("buffer_remaining", buffer.len().to_string()));
            if !buffer.is_empty() {
                self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.buffer.processing"));
                let line = std::str::from_utf8(&buffer)
                    .map_err(|_| ProviderError::InvalidResponse)?
                    .trim();
                if !line.is_empty() {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                        if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
                            return Err(ProviderError::Critical(err.to_string()));
                        }
                        if let Some(delta) = value.get("response") {
                            if let Some(text) = delta.as_str() {
                                content.push_str(text);
                            } else {
                                content.push_str(&delta.to_string());
                            }
                        }
                    } else {
                        self.logger.log(LogEvent::new(LogLevel::Warn, "ollama.json.parse.error.remaining").with_field("line", line.to_string()));
                    }
                }
            }

            Ok((content, done_received))
        }).await;

        let (content, done_received) = match streaming_result {
            Ok(Ok((c, d))) => (c, d),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(ProviderError::Timeout),
        };

        if !done_received {
            self.logger.log(LogEvent::new(LogLevel::Warn, "ollama.done.not_received"));
        }

        if content.trim().is_empty() {
            return Err(ProviderError::InvalidResponse);
        }

        self.logger.log(LogEvent::new(LogLevel::Debug, "ollama.generation.complete").with_field("content_length", content.len().to_string()));
        Ok(GenerationResult {
            provider_id: request.provider_id,
            model: model_name,
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

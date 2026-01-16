use crate::domain::ProviderId;
use crate::provider::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderError, ProviderMetadata,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptConfig {
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: Option<u64>,
}

pub struct ScriptProvider {
    id: ProviderId,
    name: String,
    config: ScriptConfig,
}

#[derive(Debug, Serialize, Deserialize)]
struct ScriptOutput {
    content: String,
    usage: Option<ScriptUsage>,
    metadata: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ScriptUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

impl ScriptProvider {
    pub fn new(id: ProviderId, config: ScriptConfig) -> Self {
        Self {
            id,
            name: "ScriptProvider".to_string(),
            config,
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            models: vec!["external".to_string()],
            max_tokens: None,
            supports_tools: false,
            capabilities: vec!["external".to_string()],
        }
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResult, ProviderError> {
        let mut cmd = Command::new(&self.config.command);
        cmd.args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|_| ProviderError::Unavailable)?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or(ProviderError::Transport)?;
        
        let input_json = serde_json::to_string(&request).map_err(|_| ProviderError::Transport)?;
        stdin
            .write_all(input_json.as_bytes())
            .await
            .map_err(|_| ProviderError::Transport)?;
        
        // Close stdin to signal end of input
        drop(child.stdin.take());

        let start = std::time::Instant::now();
        let output = child
            .wait_with_output()
            .await
            .map_err(|_| ProviderError::Transport)?;

        if !output.status.success() {
            return Err(ProviderError::InvalidResponse);
        }

        let output_str = String::from_utf8(output.stdout).map_err(|_| ProviderError::InvalidResponse)?;
        
        // Try to parse as JSON first
        let script_output: ScriptOutput = match serde_json::from_str(&output_str) {
            Ok(json) => json,
            Err(_) => {
                // If not JSON, treat raw output as content
                ScriptOutput {
                    content: output_str,
                    usage: None,
                    metadata: None,
                }
            }
        };

        let usage = script_output.usage.map(|u| crate::provider::TokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        Ok(GenerationResult {
            provider_id: request.provider_id,
            model: "external".to_string(),
            raw_output: script_output.content,
            latency: start.elapsed(),
            usage,
            metadata: script_output.metadata.unwrap_or_default(),
        })
    }
}

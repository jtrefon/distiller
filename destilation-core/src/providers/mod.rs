mod mock;
mod openrouter;
mod ollama;
mod script;

pub use mock::MockProvider;
pub use openrouter::OpenRouterProvider;
pub use ollama::OllamaProvider;
pub use script::{ScriptProvider, ScriptConfig};

#[cfg(test)]
mod tests;

use crate::provider::{ModelProvider, ProviderConfig};

pub fn create_provider(config: ProviderConfig) -> Box<dyn ModelProvider> {
    match config {
        ProviderConfig::Mock { id } => Box::new(MockProvider::new(id)),
        ProviderConfig::OpenRouter { id, base_url, api_key, model } => {
            Box::new(OpenRouterProvider::new(id, base_url, api_key, model))
        }
        ProviderConfig::Ollama { id, base_url, model } => {
            Box::new(OllamaProvider::new(id, base_url, model))
        }
        ProviderConfig::Script { id, command, args, timeout_ms } => {
            Box::new(ScriptProvider::new(id, ScriptConfig { command, args, timeout_ms }))
        }
    }
}

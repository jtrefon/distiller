mod openrouter;
mod ollama;
mod script;

pub use openrouter::OpenRouterProvider;
pub use ollama::OllamaProvider;
pub use script::{ScriptProvider, ScriptConfig};

use crate::provider::{ModelProvider, ProviderConfig};

pub fn create_provider(config: ProviderConfig) -> Box<dyn ModelProvider> {
    match config {
        ProviderConfig::OpenRouter { id, name: _, enabled: _, base_url, api_key, model } => {
            Box::new(OpenRouterProvider::new(id, base_url, api_key, model))
        }
        ProviderConfig::Ollama { id, name: _, enabled: _, base_url, model } => {
            Box::new(OllamaProvider::new(id, base_url, model))
        }
        ProviderConfig::Script { id, name: _, enabled: _, command, args, timeout_ms } => {
            Box::new(ScriptProvider::new(id, ScriptConfig { command, args, timeout_ms }))
        }
    }
}

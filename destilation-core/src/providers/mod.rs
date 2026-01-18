mod ollama;
mod openrouter;
mod script;

pub use ollama::OllamaProvider;
pub use openrouter::OpenRouterProvider;
pub use script::{ScriptConfig, ScriptProvider};

use crate::logging::SharedEventLogger;
use crate::provider::{ModelProvider, ProviderConfig};

pub fn create_provider(config: ProviderConfig, logger: SharedEventLogger) -> Box<dyn ModelProvider> {
    match config {
        ProviderConfig::OpenRouter {
            id,
            name: _,
            enabled: _,
            base_url,
            api_key,
            model,
        } => Box::new(OpenRouterProvider::new(id, base_url, api_key, model)),
        ProviderConfig::Ollama {
            id,
            name: _,
            enabled: _,
            base_url,
            model,
        } => Box::new(OllamaProvider::new(id, base_url, model, logger)),
        ProviderConfig::Script {
            id,
            name: _,
            enabled: _,
            command,
            args,
            timeout_ms,
        } => Box::new(ScriptProvider::new(
            id,
            ScriptConfig {
                command,
                args,
                timeout_ms,
            },
        )),
    }
}

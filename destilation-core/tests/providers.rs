use destilation_core::provider::ModelProvider;
use destilation_core::providers::{OllamaProvider, OpenRouterProvider};

#[test]
fn openrouter_provider_metadata() {
    let p = OpenRouterProvider::new(
        "or".to_string(),
        "https://api.openrouter.ai/api/v1".to_string(),
        "sk-test".to_string(),
        "gpt-4".to_string(),
    );
    let m = p.metadata();
    assert_eq!(m.id, "or");
    assert_eq!(m.models, vec!["gpt-4".to_string()]);
    assert!(m.supports_tools);
    assert!(m.capabilities.contains(&"reasoning".to_string()));
}

#[test]
fn ollama_provider_metadata() {
    let p = OllamaProvider::new(
        "ol".to_string(),
        "http://localhost:11434".to_string(),
        "llama2".to_string(),
    );
    let m = p.metadata();
    assert_eq!(m.id, "ol");
    assert_eq!(m.models, vec!["llama2".to_string()]);
    assert!(!m.supports_tools);
    assert!(m.capabilities.contains(&"local".to_string()));
}

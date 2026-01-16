use destilation_core::provider::ModelProvider;
use destilation_core::providers::{MockProvider, OllamaProvider, OpenRouterProvider};
use std::collections::HashMap;

#[test]
fn mock_provider_metadata() {
    let p = MockProvider::new("mock".to_string());
    let m = p.metadata();
    assert_eq!(m.id, "mock");
    assert_eq!(m.name, "MockProvider");
    assert!(m.capabilities.contains(&"general".to_string()));
}

#[tokio::test]
async fn mock_provider_generate() {
    let p = MockProvider::new("mock".to_string());
    let req = destilation_core::provider::GenerationRequest {
        provider_id: "mock".to_string(),
        model: "mock".to_string(),
        prompt: destilation_core::domain::PromptSpec {
            system: None,
            user: "hello".to_string(),
            extra: Default::default(),
        },
        temperature: None,
        top_p: None,
        max_tokens: None,
        metadata: HashMap::new(),
    };
    let res = p.generate(req).await.unwrap();
    assert_eq!(res.model, "mock");
    assert!(!res.raw_output.is_empty());
}

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

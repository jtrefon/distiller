use destilation_core::domain::PromptSpec;
use destilation_core::logging::NoopEventLogger;
use destilation_core::provider::{GenerationRequest, ModelProvider};
use destilation_core::providers::{OllamaProvider, OpenRouterProvider};
use httpmock::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

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
    let logger = std::sync::Arc::new(destilation_core::logging::NoopEventLogger);
    let p = OllamaProvider::new(
        "ol".to_string(),
        "http://localhost:11434".to_string(),
        "llama2".to_string(),
        logger,
    );
    let m = p.metadata();
    assert_eq!(m.id, "ol");
    assert_eq!(m.models, vec!["llama2".to_string()]);
    assert!(!m.supports_tools);
    assert!(m.capabilities.contains(&"local".to_string()));
}

#[tokio::test]
async fn ollama_streaming_success() {
    let server = MockServer::start();
    let logger = Arc::new(NoopEventLogger);

    // Mock the /api/tags endpoint
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200).json_body(serde_json::json!({
            "models": [{"name": "llama2", "size": 1000000}]
        }));
    });

    // Mock the /api/generate endpoint with streaming response
    server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"response":"Hello","done":false}
{"response":" world","done":false}
{"response":"!","done":true}
"#);
    });

    let client = reqwest::Client::builder()
        .build()
        .unwrap();

    let provider = OllamaProvider::with_client(
        "test".to_string(),
        server.base_url(),
        "llama2".to_string(),
        client,
        logger,
    );

    let request = GenerationRequest {
        provider_id: "test".to_string(),
        model: "llama2".to_string(),
        prompt: PromptSpec {
            system: None,
            user: "Say hello".to_string(),
            extra: HashMap::new(),
        },
        max_tokens: None,
        temperature: None,
        top_p: None,
        metadata: HashMap::new(),
    };

    let result: destilation_core::provider::GenerationResult = provider.generate(request).await.unwrap();
    assert_eq!(result.raw_output, "Hello world!");
    assert!(result.latency > std::time::Duration::from_millis(0));
}

#[tokio::test]
async fn ollama_streaming_with_error() {
    let server = MockServer::start();
    let logger = Arc::new(NoopEventLogger);

    // Mock the /api/tags endpoint
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200).json_body(serde_json::json!({
            "models": [{"name": "llama2", "size": 1000000}]
        }));
    });

    // Mock the /api/generate endpoint with error in stream
    server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"response":"Hello","done":false}
{"error":"Model not found","done":true}
"#);
    });

    let client = reqwest::Client::builder()
        .build()
        .unwrap();

    let provider = OllamaProvider::with_client(
        "test".to_string(),
        server.base_url(),
        "llama2".to_string(),
        client,
        logger,
    );

    let request = GenerationRequest {
        provider_id: "test".to_string(),
        model: "llama2".to_string(),
        prompt: PromptSpec {
            system: None,
            user: "Say hello".to_string(),
            extra: HashMap::new(),
        },
        max_tokens: None,
        temperature: None,
        top_p: None,
        metadata: HashMap::new(),
    };

    let result: Result<destilation_core::provider::GenerationResult, destilation_core::provider::ProviderError> = provider.generate(request).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        destilation_core::provider::ProviderError::Critical(msg) => {
            assert_eq!(msg, "Model not found");
        }
        _ => panic!("Expected Critical error"),
    }
}

#[tokio::test]
async fn ollama_streaming_incomplete_json_fallback() {
    let server = MockServer::start();
    let logger = Arc::new(NoopEventLogger);

    // Mock the /api/tags endpoint
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200).json_body(serde_json::json!({
            "models": [{"name": "llama2", "size": 1000000}]
        }));
    });

    // Mock the /api/generate endpoint with incomplete JSON (no done signal)
    server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"response":"Hello","done":false}
{"response":" world","done":false}
{"response":"!"}"#); // Missing done: true
    });

    let client = reqwest::Client::builder()
        .build()
        .unwrap();

    let provider = OllamaProvider::with_client(
        "test".to_string(),
        server.base_url(),
        "llama2".to_string(),
        client,
        logger,
    );

    let request = GenerationRequest {
        provider_id: "test".to_string(),
        model: "llama2".to_string(),
        prompt: PromptSpec {
            system: None,
            user: "Say hello".to_string(),
            extra: HashMap::new(),
        },
        max_tokens: None,
        temperature: None,
        top_p: None,
        metadata: HashMap::new(),
    };

    let result: destilation_core::provider::GenerationResult = provider.generate(request).await.unwrap();
    assert_eq!(result.raw_output, "Hello world!");
}

#[tokio::test]
async fn ollama_streaming_timeout() {
    let server = MockServer::start();
    let logger = Arc::new(NoopEventLogger);

    // Mock the /api/tags endpoint
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200).json_body(serde_json::json!({
            "models": [{"name": "llama2", "size": 1000000}]
        }));
    });

    // Mock the /api/generate endpoint with slow response
    server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .delay(std::time::Duration::from_secs(2)) // Longer than 1s stream timeout used in this test
            .body(r#"{"response":"Hello","done":true}
"#);
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5)) // Client timeout longer than stream timeout
        .build()
        .unwrap();

    let provider = OllamaProvider::with_client_and_timeout(
        "test".to_string(),
        server.base_url(),
        "llama2".to_string(),
        client,
        logger,
        std::time::Duration::from_secs(1), // 1s stream timeout for fast test
    );

    let request = GenerationRequest {
        provider_id: "test".to_string(),
        model: "llama2".to_string(),
        prompt: PromptSpec {
            system: None,
            user: "Say hello".to_string(),
            extra: HashMap::new(),
        },
        max_tokens: None,
        temperature: None,
        top_p: None,
        metadata: HashMap::new(),
    };

    let result: Result<destilation_core::provider::GenerationResult, destilation_core::provider::ProviderError> = provider.generate(request).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        destilation_core::provider::ProviderError::Timeout => {
            // Expected timeout
        }
        _ => panic!("Expected Timeout error"),
    }
}

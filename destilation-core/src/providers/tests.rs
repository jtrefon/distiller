#[cfg(test)]
mod tests {
    use crate::domain::PromptSpec;
    use crate::provider::{GenerationRequest, ProviderConfig};
    use crate::providers::create_provider;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_mock_provider() {
        let config = ProviderConfig::Mock {
            id: "test-mock".to_string(),
        };
        let provider = create_provider(config);
        let meta = provider.metadata();
        assert_eq!(meta.id, "test-mock");
        assert_eq!(meta.name, "MockProvider");

        let req = GenerationRequest {
            provider_id: "test-mock".to_string(),
            model: "mock".to_string(),
            prompt: PromptSpec {
                system: None,
                user: "test".to_string(),
                extra: HashMap::new(),
            },
            max_tokens: None,
            temperature: None,
            top_p: None,
            metadata: HashMap::new(),
        };

        let res = provider.generate(req).await.unwrap();
        assert_eq!(res.model, "mock");
    }

    #[tokio::test]
    async fn test_script_provider_echo() {
        // This test relies on 'echo' command being available
        let output_json = serde_json::json!({
            "content": "Hello from script",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            },
            "metadata": {
                "version": "1.0"
            }
        }).to_string();

        // Use a command that prints the JSON to stdout
        // For portability, we might need 'sh -c echo ...'
        let config = ProviderConfig::Script {
            id: "test-script".to_string(),
            command: "echo".to_string(),
            args: vec![output_json],
            timeout_ms: None,
        };

        let provider = create_provider(config);
        let meta = provider.metadata();
        assert_eq!(meta.id, "test-script");
        assert_eq!(meta.models, vec!["external"]);

        let req = GenerationRequest {
            provider_id: "test-script".to_string(),
            model: "external".to_string(),
            prompt: PromptSpec {
                system: None,
                user: "ignored".to_string(),
                extra: HashMap::new(),
            },
            max_tokens: None,
            temperature: None,
            top_p: None,
            metadata: HashMap::new(),
        };

        let res = provider.generate(req).await.unwrap();
        assert_eq!(res.raw_output, "Hello from script");
        assert!(res.usage.is_some());
        assert_eq!(res.usage.unwrap().total_tokens, 15);
        assert_eq!(res.metadata.get("version").unwrap(), "1.0");
    }
    
    #[test]
    fn test_provider_config_serialization() {
        let config = ProviderConfig::OpenRouter {
            id: "or".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key: "sk-123".to_string(),
            model: "gpt-4".to_string(),
        };
        
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();
        
        match deserialized {
            ProviderConfig::OpenRouter { id, .. } => assert_eq!(id, "or"),
            _ => panic!("Wrong variant"),
        }
    }
}

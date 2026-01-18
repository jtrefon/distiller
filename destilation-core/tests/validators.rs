use destilation_core::domain::{TemplateConfig, TemplateMode, TemplateSchema, TemplateSchemaField};
use destilation_core::provider::GenerationResult;
use destilation_core::validation::ValidationContext;
use destilation_core::validation::Validator;
use destilation_core::validators::{DedupValidator, SemanticDedupValidator, StructuralValidator};

fn mk_template() -> TemplateConfig {
    TemplateConfig {
        id: "simple_qa".to_string(),
        name: "Simple Q&A".to_string(),
        description: "Short question-answer pairs".to_string(),
        mode: TemplateMode::Simple,
        schema: TemplateSchema {
            version: "v1".to_string(),
            fields: vec![
                TemplateSchemaField {
                    name: "question".to_string(),
                    field_type: "string".to_string(),
                    required: true,
                },
                TemplateSchemaField {
                    name: "answer".to_string(),
                    field_type: "string".to_string(),
                    required: true,
                },
            ],
            json_schema: None,
        },
        system_prompt: "".to_string(),
        user_prompt_pattern: "".to_string(),
        examples: vec![],
        validators: vec!["structural".to_string()],
    }
}

fn mk_ctx(parsed: serde_json::Value) -> ValidationContext {
    let job = destilation_core::domain::Job {
        id: "job".to_string(),
        config: destilation_core::domain::JobConfig {
            id: "job".to_string(),
            name: "Job".to_string(),
            description: None,
            target_samples: 1,
            max_concurrency: 1,
            domains: vec![],
            template_id: "simple_qa".to_string(),
            reasoning_mode: destilation_core::domain::ReasoningMode::Simple,
            providers: vec![],
            validation: destilation_core::domain::JobValidationConfig {
                max_attempts: 1,
                validators: vec![],
                min_quality_score: None,
                fail_fast: true,
            },
            output: destilation_core::domain::JobOutputConfig {
                dataset_dir: "datasets/test".to_string(),
                shard_size: 1,
                compress: false,
                metadata: Default::default(),
            },
        },
        status: destilation_core::domain::JobStatus::Pending,
        created_at: chrono::Utc::now(),
        started_at: None,
        updated_at: chrono::Utc::now(),
        finished_at: None,
        completed_samples: 0,
    };
    let task = destilation_core::domain::Task {
        id: "task".to_string(),
        job_id: "job".to_string(),
        state: destilation_core::domain::TaskState::Validating,
        attempts: 0,
        max_attempts: 1,
        provider_id: Some("mock".to_string()),
        domain_id: "algorithms".to_string(),
        template_id: "simple_qa".to_string(),
        prompt_spec: destilation_core::domain::PromptSpec {
            system: None,
            user: "".to_string(),
            extra: Default::default(),
        },
        raw_response: None,
        validation_result: None,
        quality_score: None,
        is_negative: false,
    };
    let provider_result = GenerationResult {
        provider_id: "mock".to_string(),
        model: "mock".to_string(),
        raw_output: parsed.to_string(),
        latency: std::time::Duration::from_millis(1),
        usage: None,
        metadata: Default::default(),
    };
    ValidationContext {
        job,
        task,
        template: mk_template(),
        provider_result,
        parsed_output: Some(parsed),
    }
}

#[test]
fn structural_validator_passes_valid_sample() {
    let v = StructuralValidator::new();
    let parsed = serde_json::json!({"question":"Q","answer":"A"});
    let ctx = mk_ctx(parsed);
    let outcome = v.validate(&ctx);
    assert!(outcome.passed);
}

#[test]
fn structural_validator_checks_all_types() {
    let v = StructuralValidator::new();

    // 1. Missing field
    let p1 = serde_json::json!({"question": "Q"});
    let ctx1 = mk_ctx(p1);
    // Note: mk_ctx uses mk_template() which returns the schema.
    let out1 = v.validate(&ctx1);
    assert!(!out1.passed);
    assert!(out1.issues.iter().any(|i| i.code == "schema.required"));

    // 2. Wrong type (number instead of string)
    let p2 = serde_json::json!({"question": 123, "answer": "A"});
    let ctx2 = mk_ctx(p2);
    let out2 = v.validate(&ctx2);
    assert!(!out2.passed);
    assert!(out2.issues.iter().any(|i| i.code == "schema.type"));

    // 3. Not an object
    let p3 = serde_json::json!([1, 2]);
    let ctx3 = mk_ctx(p3);
    let out3 = v.validate(&ctx3);
    assert!(!out3.passed);
    assert!(out3.issues.iter().any(|i| i.code == "schema.object"));
}

#[test]
fn structural_validator_complex_types() {
    // Create template with array/number/object fields
    let mut t = mk_template();
    t.schema.fields.push(TemplateSchemaField {
        name: "scores".to_string(),
        field_type: "array".to_string(),
        required: true,
    });
    t.schema.fields.push(TemplateSchemaField {
        name: "count".to_string(),
        field_type: "number".to_string(),
        required: true,
    });
    t.schema.fields.push(TemplateSchemaField {
        name: "meta".to_string(),
        field_type: "object".to_string(),
        required: true,
    });

    let v = StructuralValidator::new();

    // Valid case
    let p_valid = serde_json::json!({
        "question": "Q",
        "answer": "A",
        "scores": [1, 2],
        "count": 10,
        "meta": {"foo": "bar"}
    });
    let mut ctx = mk_ctx(p_valid);
    ctx.template = t.clone();
    let out = v.validate(&ctx);
    assert!(
        out.passed,
        "Complex types valid case failed: {:?}",
        out.issues
    );

    // Invalid array
    let p_inv_arr = serde_json::json!({
        "question": "Q", "answer": "A", "scores": "bad", "count": 10, "meta": {}
    });
    let mut ctx_inv = mk_ctx(p_inv_arr);
    ctx_inv.template = t.clone();
    let out_inv = v.validate(&ctx_inv);
    assert!(!out_inv.passed);

    // Invalid number
    let p_inv_num = serde_json::json!({
        "question": "Q", "answer": "A", "scores": [], "count": "bad", "meta": {}
    });
    let mut ctx_num = mk_ctx(p_inv_num);
    ctx_num.template = t.clone();
    let out_num = v.validate(&ctx_num);
    assert!(!out_num.passed);

    // Invalid object
    let p_inv_obj = serde_json::json!({
        "question": "Q", "answer": "A", "scores": [], "count": 10, "meta": 1
    });
    let mut ctx_obj = mk_ctx(p_inv_obj);
    ctx_obj.template = t.clone();
    let out_obj = v.validate(&ctx_obj);
    assert!(!out_obj.passed);
}

#[test]
fn semantic_dedup_validator_blocks_similar() {
    let v = SemanticDedupValidator::new(0.5);

    // Sample 1: "The quick brown fox"
    let parsed1 = serde_json::json!({"text": "The quick brown fox"});
    let ctx1 = mk_ctx(parsed1);
    let out1 = v.validate(&ctx1);
    assert!(out1.passed);

    // Sample 2: "The quick brown fox jumps" (Similar)
    // Intersection: 4 (the, quick, brown, fox)
    // Union: 5 (the, quick, brown, fox, jumps)
    // Jaccard: 4/5 = 0.8 -> should fail if threshold is 0.5
    let parsed2 = serde_json::json!({"text": "The quick brown fox jumps"});
    let ctx2 = mk_ctx(parsed2);
    let out2 = v.validate(&ctx2);
    assert!(!out2.passed);

    // Sample 3: "Different content entirely" (Dissimilar)
    let parsed3 = serde_json::json!({"text": "Different content entirely"});
    let ctx3 = mk_ctx(parsed3);
    let out3 = v.validate(&ctx3);
    assert!(out3.passed);
}

#[test]
fn dedup_validator_blocks_duplicates() {
    let v = DedupValidator::new();
    let parsed1 = serde_json::json!({"question":"Q","answer":"A"});
    let ctx1 = mk_ctx(parsed1.clone());
    let out1 = v.validate(&ctx1);
    assert!(out1.passed);
    let ctx2 = mk_ctx(parsed1);
    let out2 = v.validate(&ctx2);
    assert!(!out2.passed);
}

use crate::domain::TemplateConfig;
use crate::validation::{
    ValidationContext, ValidationIssue, ValidationOutcome, ValidationSeverity, Validator,
};
use jsonschema::Validator as JsonSchemaValidator;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Mutex;

pub struct StructuralValidator;

impl StructuralValidator {
    pub fn new() -> Self {
        Self
    }
}

pub struct SemanticDedupValidator {
    seen_tokens: Mutex<Vec<HashSet<String>>>,
    threshold: f32,
}

impl SemanticDedupValidator {
    pub fn new(threshold: f32) -> Self {
        Self {
            seen_tokens: Mutex::new(Vec::new()),
            threshold,
        }
    }

    fn tokenize(text: &str) -> HashSet<String> {
        text.to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect()
    }

    fn jaccard_index(set1: &HashSet<String>, set2: &HashSet<String>) -> f32 {
        if set1.is_empty() && set2.is_empty() {
            return 1.0;
        }
        let intersection = set1.intersection(set2).count();
        let union = set1.union(set2).count();
        if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        }
    }
}

impl Default for SemanticDedupValidator {
    fn default() -> Self {
        Self::new(0.8)
    }
}

impl Validator for SemanticDedupValidator {
    fn id(&self) -> &str {
        "semantic_dedup"
    }

    fn validate(&self, ctx: &ValidationContext) -> ValidationOutcome {
        if let Some(parsed) = &ctx.parsed_output {
            // Convert the entire JSON to a string for tokenization.
            // In a real scenario, we might want to target specific fields.
            let text = serde_json::to_string(parsed).unwrap_or_default();
            let tokens = Self::tokenize(&text);

            let mut guard = self.seen_tokens.lock().unwrap();

            for seen in guard.iter() {
                let sim = Self::jaccard_index(&tokens, seen);
                if sim >= self.threshold {
                    let mut details = HashMap::new();
                    details.insert(
                        "reason".to_string(),
                        "semantically similar sample exists".to_string(),
                    );
                    details.insert("similarity".to_string(), format!("{:.2}", sim));
                    return ValidationOutcome {
                        passed: false,
                        issues: vec![ValidationIssue {
                            code: "dedup.semantic".to_string(),
                            message: "duplicate detected (semantic)".to_string(),
                            severity: ValidationSeverity::Error,
                            details,
                        }],
                        score: Some(0.0),
                    };
                }
            }

            guard.push(tokens);

            ValidationOutcome {
                passed: true,
                issues: vec![],
                score: Some(1.0),
            }
        } else {
            ValidationOutcome {
                passed: true,
                issues: vec![],
                score: Some(0.5),
            }
        }
    }
}

impl Default for StructuralValidator {
    fn default() -> Self {
        Self::new()
    }
}

fn check_schema(template: &TemplateConfig, value: &Value) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    // 1. JSON Schema validation (if provided)
    if let Some(schema) = &template.schema.json_schema {
        match JsonSchemaValidator::new(schema) {
            Ok(validator) => {
                for err in validator.iter_errors(value) {
                    let details = HashMap::new();
                    issues.push(ValidationIssue {
                        code: "schema.json".to_string(),
                        message: err.to_string(),
                        severity: ValidationSeverity::Error,
                        details,
                    });
                }
            }
            Err(e) => {
                let mut d = HashMap::new();
                d.insert("error".to_string(), e.to_string());
                issues.push(ValidationIssue {
                    code: "schema.invalid_definition".to_string(),
                    message: "template has invalid JSON schema".to_string(),
                    severity: ValidationSeverity::Warning,
                    details: d,
                });
            }
        }
    }

    // 2. Legacy field-based validation (always run as fallback/addition)
    let obj = value.as_object();
    if obj.is_none() {
        issues.push(ValidationIssue {
            code: "schema.object".to_string(),
            message: "output is not a JSON object".to_string(),
            severity: ValidationSeverity::Error,
            details: HashMap::new(),
        });
        return issues;
    }
    let obj = obj.unwrap();
    for field in &template.schema.fields {
        if field.required && !obj.contains_key(&field.name) {
            let mut d = HashMap::new();
            d.insert("field".to_string(), field.name.clone());
            issues.push(ValidationIssue {
                code: "schema.required".to_string(),
                message: "required field missing".to_string(),
                severity: ValidationSeverity::Error,
                details: d,
            });
        }
        if let Some(v) = obj.get(&field.name) {
            match field.field_type.as_str() {
                "string" => {
                    if !v.is_string() {
                        let mut d = HashMap::new();
                        d.insert("field".to_string(), field.name.clone());
                        d.insert("expected".to_string(), "string".to_string());
                        issues.push(ValidationIssue {
                            code: "schema.type".to_string(),
                            message: "field has wrong type".to_string(),
                            severity: ValidationSeverity::Error,
                            details: d,
                        });
                    }
                }
                "number" => {
                    if !v.is_number() {
                        let mut d = HashMap::new();
                        d.insert("field".to_string(), field.name.clone());
                        d.insert("expected".to_string(), "number".to_string());
                        issues.push(ValidationIssue {
                            code: "schema.type".to_string(),
                            message: "field has wrong type".to_string(),
                            severity: ValidationSeverity::Error,
                            details: d,
                        });
                    }
                }
                "array" => {
                    if !v.is_array() {
                        let mut d = HashMap::new();
                        d.insert("field".to_string(), field.name.clone());
                        d.insert("expected".to_string(), "array".to_string());
                        issues.push(ValidationIssue {
                            code: "schema.type".to_string(),
                            message: "field has wrong type".to_string(),
                            severity: ValidationSeverity::Error,
                            details: d,
                        });
                    }
                }
                "object" => {
                    if !v.is_object() {
                        let mut d = HashMap::new();
                        d.insert("field".to_string(), field.name.clone());
                        d.insert("expected".to_string(), "object".to_string());
                        issues.push(ValidationIssue {
                            code: "schema.type".to_string(),
                            message: "field has wrong type".to_string(),
                            severity: ValidationSeverity::Error,
                            details: d,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    issues
}

impl Validator for StructuralValidator {
    fn id(&self) -> &str {
        "structural"
    }

    fn validate(&self, ctx: &ValidationContext) -> ValidationOutcome {
        if let Some(parsed) = &ctx.parsed_output {
            let issues = check_schema(&ctx.template, parsed);
            let passed = issues.is_empty();
            ValidationOutcome {
                passed,
                issues,
                score: if passed { Some(1.0) } else { Some(0.0) },
            }
        } else {
            let mut d = HashMap::new();
            d.insert("reason".to_string(), "parse failed".to_string());
            ValidationOutcome {
                passed: false,
                issues: vec![ValidationIssue {
                    code: "parse".to_string(),
                    message: "failed to parse provider output as JSON".to_string(),
                    severity: ValidationSeverity::Error,
                    details: d,
                }],
                score: Some(0.0),
            }
        }
    }
}

pub struct DedupValidator {
    seen: Mutex<HashSet<String>>,
}

impl DedupValidator {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
        }
    }
}

impl Default for DedupValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Validator for DedupValidator {
    fn id(&self) -> &str {
        "dedup"
    }

    fn validate(&self, ctx: &ValidationContext) -> ValidationOutcome {
        if let Some(parsed) = &ctx.parsed_output {
            let key = serde_json::to_string(parsed).unwrap_or_default();
            let mut guard = self.seen.lock().unwrap();
            if guard.contains(&key) {
                let mut d = HashMap::new();
                d.insert("reason".to_string(), "duplicate sample".to_string());
                return ValidationOutcome {
                    passed: false,
                    issues: vec![ValidationIssue {
                        code: "dedup.duplicate".to_string(),
                        message: "duplicate detected".to_string(),
                        severity: ValidationSeverity::Error,
                        details: d,
                    }],
                    score: Some(0.0),
                };
            }
            guard.insert(key);
            ValidationOutcome {
                passed: true,
                issues: vec![],
                score: Some(1.0),
            }
        } else {
            ValidationOutcome {
                passed: true,
                issues: vec![],
                score: Some(0.5),
            }
        }
    }
}

pub mod tui;
use crate::tui::Tui;
use clap::Parser;
use destilation_core::domain::{
    DomainSpec, JobConfig, JobOutputConfig, JobProviderSpec, JobValidationConfig, ReasoningMode,
    TemplateConfig, TemplateId, TemplateMode, TemplateSchema, TemplateSchemaField,
};
use destilation_core::metrics::{InMemoryMetrics, Metrics};
use destilation_core::orchestrator::Orchestrator;
use destilation_core::providers::MockProvider;
use destilation_core::storage::{FilesystemDatasetWriter, InMemoryJobStore, InMemoryTaskStore};
use destilation_core::providers::{OllamaProvider, OpenRouterProvider};
use destilation_core::validation::Validator;
use destilation_core::validators::DedupValidator;
use destilation_core::validators::SemanticDedupValidator;
use destilation_core::validators::StructuralValidator;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Parser)]
pub struct Cli {
    #[arg(long)]
    pub config: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    run_default(cli.config).await.expect("run failed");
}

#[derive(serde::Deserialize)]
struct GlobalConfig {
    runtime: Option<RuntimeConfig>,
    providers: Option<ProvidersConfig>,
    templates: Option<HashMap<String, TemplateToml>>,
    jobs: Option<Vec<JobToml>>,
}

#[derive(serde::Deserialize)]
struct RuntimeConfig {
    max_global_concurrency: Option<u32>,
    dataset_root: Option<String>,
}

#[derive(serde::Deserialize)]
struct ProvidersConfig {
    openrouter: Option<OpenRouterConfig>,
    ollama: Option<OllamaConfig>,
}

#[derive(serde::Deserialize)]
struct OpenRouterConfig {
    base_url: String,
    api_key_env: String,
    model: String,
}

#[derive(serde::Deserialize)]
struct OllamaConfig {
    base_url: String,
    model: String,
}

#[derive(serde::Deserialize)]
struct TemplateToml {
    id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    mode: Option<String>,
    system_prompt: Option<String>,
    user_prompt_pattern: Option<String>,
    validators: Option<Vec<String>>,
    schema: Option<TemplateSchemaToml>,
}

#[derive(serde::Deserialize)]
struct TemplateSchemaToml {
    version: Option<String>,
    fields: Option<Vec<TemplateFieldToml>>,
}

#[derive(Clone, serde::Deserialize)]
struct TemplateFieldToml {
    name: String,
    field_type: String,
    required: bool,
}

#[derive(serde::Deserialize)]
struct JobToml {
    id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    target_samples: Option<u64>,
    max_concurrency: Option<u32>,
    template_id: Option<String>,
    validators: Option<Vec<String>>,
    providers: Option<Vec<String>>,
}

fn load_global_config(path: Option<String>) -> Option<GlobalConfig> {
    let path = path.unwrap_or_else(|| "config.toml".to_string());
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str::<GlobalConfig>(&s).ok(),
        Err(_) => None,
    }
}

async fn run_default(config_path: Option<String>) -> anyhow::Result<()> {
    let job_store = Arc::new(InMemoryJobStore::new());
    let task_store = Arc::new(InMemoryTaskStore::new());
    let gc = load_global_config(config_path);
    let dataset_root = gc
        .as_ref()
        .and_then(|g| g.runtime.as_ref().and_then(|r| r.dataset_root.clone()))
        .unwrap_or_else(|| "datasets/default".to_string());
    let _global_concurrency = gc
        .as_ref()
        .and_then(|g| g.runtime.as_ref().and_then(|r| r.max_global_concurrency));
    let dataset_writer = Arc::new(FilesystemDatasetWriter::new(dataset_root.clone()));

    let mut templates = HashMap::new();
    if let Some(tconf) = gc.as_ref().and_then(|g| g.templates.as_ref()) {
        for t in tconf.values() {
            let id = t.id.clone().unwrap_or_else(|| "custom".to_string());
            let mode = match t.mode.as_deref() {
                Some("Simple") => TemplateMode::Simple,
                Some("Moe") => TemplateMode::Moe,
                Some("Reasoning") => TemplateMode::Reasoning,
                Some("Tools") => TemplateMode::Tools,
                _ => TemplateMode::Custom,
            };
            let schema = if let Some(s) = &t.schema {
                TemplateSchema {
                    version: s.version.clone().unwrap_or_else(|| "v1".to_string()),
                    fields: s
                        .fields
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|f| TemplateSchemaField {
                            name: f.name,
                            field_type: f.field_type,
                            required: f.required,
                        })
                        .collect(),
                    json_schema: None,
                }
            } else {
                TemplateSchema {
                    version: "v1".to_string(),
                    fields: vec![],
                    json_schema: None,
                }
            };
            let tc = TemplateConfig {
                id: TemplateId::from(id.clone()),
                name: t.name.clone().unwrap_or_else(|| id.clone()),
                description: t
                    .description
                    .clone()
                    .unwrap_or_else(|| "User-defined template".to_string()),
                mode,
                schema,
                system_prompt: t.system_prompt.clone().unwrap_or_else(|| "".to_string()),
                user_prompt_pattern: t
                    .user_prompt_pattern
                    .clone()
                    .unwrap_or_else(|| "".to_string()),
                examples: vec![],
                validators: t
                    .validators
                    .clone()
                    .unwrap_or_else(|| vec!["structural".to_string()]),
            };
            templates.insert(tc.id.clone(), tc);
        }
    }
    if templates.is_empty() {
        let template = TemplateConfig {
            id: TemplateId::from("simple_qa"),
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
            system_prompt: "You are generating high-quality Q&A pairs.".to_string(),
            user_prompt_pattern: "Generate a Q&A pair about algorithms.".to_string(),
            examples: vec![],
            validators: vec!["structural".to_string()],
        };
        templates.insert(template.id.clone(), template.clone());
    }

    let mut providers: HashMap<String, Arc<dyn destilation_core::provider::ModelProvider>> =
        HashMap::new();
    let mock = Arc::new(MockProvider::new("mock".to_string()));
    providers.insert("mock".to_string(), mock);
    if let Some(pconf) = gc.as_ref().and_then(|g| g.providers.as_ref()) {
        if let Some(or) = &pconf.openrouter {
            if let Ok(key) = std::env::var(&or.api_key_env) {
                let p = Arc::new(OpenRouterProvider::new(
                    "openrouter".to_string(),
                    or.base_url.clone(),
                    key,
                    or.model.clone(),
                ));
                providers.insert("openrouter".to_string(), p);
            }
        }
        if let Some(ol) = &pconf.ollama {
            let p = Arc::new(OllamaProvider::new(
                "ollama".to_string(),
                ol.base_url.clone(),
                ol.model.clone(),
            ));
            providers.insert("ollama".to_string(), p);
        }
    }

    let mut validators: Vec<Arc<dyn Validator>> = vec![Arc::new(StructuralValidator::new())];
    if let Some(jobs) = gc.as_ref().and_then(|g| g.jobs.as_ref()) {
        if let Some(job) = jobs.first() {
            let vset = job.validators.clone().unwrap_or_default();
            if vset.iter().any(|v| v == "dedup") {
                validators.push(Arc::new(DedupValidator::new()));
            }
            if vset.iter().any(|v| v == "semantic_dedup") {
                validators.push(Arc::new(SemanticDedupValidator::default()));
            }
        }
    } else {
        validators.push(Arc::new(DedupValidator::new()));
    }
    let selected_template_id = if let Some(jobs) = gc.as_ref().and_then(|g| g.jobs.as_ref()) {
        if let Some(job) = jobs.first() {
            if let Some(tid) = &job.template_id {
                TemplateId::from(tid.clone())
            } else {
                templates
                    .keys()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| TemplateId::from("simple_qa"))
            }
        } else {
            TemplateId::from("simple_qa")
        }
    } else {
        templates
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| TemplateId::from("simple_qa"))
    };

    let (job_id, job_name, job_desc, target_samples, max_concurrency, job_providers) =
        if let Some(jobs) = gc.as_ref().and_then(|g| g.jobs.as_ref()) {
            if let Some(job) = jobs.first() {
                (
                    job.id
                        .clone()
                        .unwrap_or_else(|| "job-default-001".to_string()),
                    job.name
                        .clone()
                        .unwrap_or_else(|| "Default Job".to_string()),
                    job.description
                        .clone()
                        .or_else(|| Some("Default distillation job".to_string())),
                    job.target_samples.unwrap_or(3),
                    job.max_concurrency.unwrap_or(1),
                    build_job_providers(&providers, job.providers.as_ref()),
                )
            } else {
                (
                    "job-default-001".to_string(),
                    "Default Job".to_string(),
                    Some("Default distillation job".to_string()),
                    3,
                    1,
                    build_job_providers(&providers, None),
                )
            }
        } else {
            (
                "job-default-001".to_string(),
                "Default Job".to_string(),
                Some("Default distillation job".to_string()),
                3,
                1,
                build_job_providers(&providers, None),
            )
        };

    let metrics = Arc::new(InMemoryMetrics::new());
    let orchestrator = Orchestrator {
        job_store,
        task_store,
        providers,
        templates,
        validators,
        dataset_writer,
        metrics: metrics.clone(),
    };

    let dataset_dir = format!("{dataset_root}/{job_id}");

    let job_config = JobConfig {
        id: job_id,
        name: job_name,
        description: job_desc,
        target_samples,
        max_concurrency,
        domains: vec![DomainSpec {
            id: "algorithms".to_string(),
            name: "Algorithms".to_string(),
            weight: 1.0,
            tags: vec!["cs".to_string()],
        }],
        template_id: selected_template_id,
        reasoning_mode: ReasoningMode::Simple,
        providers: job_providers,
        validation: JobValidationConfig {
            max_attempts: 2,
            validators: vec!["structural".to_string(), "dedup".to_string()],
            min_quality_score: Some(0.8),
            fail_fast: true,
        },
        output: JobOutputConfig {
            dataset_dir,
            shard_size: 1000,
            compress: false,
            metadata: HashMap::new(),
        },
    };

    let job = orchestrator.submit_job(job_config).await?;
    let job_id = job.id.clone();
    let orchestrator_clone = orchestrator.clone();
    let metrics_clone = metrics.clone();

    let handle = tokio::spawn(async move { orchestrator_clone.run_job(&job_id).await });

    let tui = Tui::new(metrics_clone);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_clone = done.clone();
    tokio::spawn(async move {
        let _ = handle.await;
        done_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let tui_res = tokio::task::spawn_blocking(move || {
        tui.run(|| done.load(std::sync::atomic::Ordering::Relaxed))
    })
    .await?;

    tui_res?;

    let snap = metrics.snapshot();
    println!(
        "metrics: jobs_submitted={} tasks_enqueued={} tasks_started={} tasks_persisted={} tasks_rejected={} samples_persisted={} validator_pass={} validator_fail={}",
        snap.jobs_submitted,
        snap.tasks_enqueued,
        snap.tasks_started,
        snap.tasks_persisted,
        snap.tasks_rejected,
        snap.samples_persisted,
        snap.validator_pass,
        snap.validator_fail
    );
    Ok(())
}

fn build_job_providers(
    all_providers: &HashMap<String, Arc<dyn destilation_core::provider::ModelProvider>>,
    preferred: Option<&Vec<String>>,
) -> Vec<JobProviderSpec> {
    let mut specs = Vec::new();
    let ids: Vec<String> = if let Some(list) = preferred {
        list.iter()
            .filter(|id| all_providers.contains_key(*id))
            .cloned()
            .collect()
    } else {
        let mut v = Vec::new();
        if all_providers.contains_key("mock") {
            v.push("mock".to_string());
        }
        if all_providers.contains_key("openrouter") {
            v.push("openrouter".to_string());
        }
        if all_providers.contains_key("ollama") {
            v.push("ollama".to_string());
        }
        v
    };

    for id in ids {
        let caps = match id.as_str() {
            "openrouter" | "ollama" => vec!["reasoning".to_string()],
            _ => vec!["general".to_string()],
        };
        specs.push(JobProviderSpec {
            provider_id: id,
            weight: 1.0,
            capabilities_required: caps,
        });
    }

    if specs.is_empty() && all_providers.contains_key("mock") {
        specs.push(JobProviderSpec {
            provider_id: "mock".to_string(),
            weight: 1.0,
            capabilities_required: vec!["general".to_string()],
        });
    }

    specs
}

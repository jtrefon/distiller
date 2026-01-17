pub mod tui;
use crate::tui::{Tui, TuiCommand, TuiUpdate};
use clap::Parser;
use destilation_core::domain::{
    TemplateConfig, TemplateId, TemplateMode, TemplateSchema, TemplateSchemaField,
};
use destilation_core::logging::BufferedFileEventLogger;
use destilation_core::metrics::{InMemoryMetrics, Metrics};
use destilation_core::orchestrator::Orchestrator;
use destilation_core::storage::{
    DirectoryJobStore, DirectoryProviderStore, DirectoryTaskStore, DirectoryTemplateStore,
    FilesystemDatasetWriter, JobStore, ProviderStore,
};
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
    dataset_root: Option<String>,
}

use destilation_core::provider::ProviderConfig;

#[derive(serde::Deserialize)]
struct ProvidersConfig {
    openrouter: Option<OpenRouterConfig>,
    ollama: Option<OllamaConfig>,
    scripts: Option<HashMap<String, ScriptProviderConfig>>,
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
struct ScriptProviderConfig {
    command: String,
    args: Option<Vec<String>>,
    timeout_ms: Option<u64>,
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
    validators: Option<Vec<String>>,
}

fn load_global_config(path: Option<String>) -> Option<GlobalConfig> {
    let path = path.unwrap_or_else(|| "config.toml".to_string());
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str::<GlobalConfig>(&s).ok(),
        Err(_) => None,
    }
}

async fn run_default(config_path: Option<String>) -> anyhow::Result<()> {
    let gc = load_global_config(config_path);

    let dataset_root = gc
        .as_ref()
        .and_then(|g| g.runtime.as_ref().and_then(|r| r.dataset_root.clone()))
        .unwrap_or_else(|| "datasets/default".to_string());

    let job_store = Arc::new(DirectoryJobStore::new(dataset_root.clone()));
    let task_store = Arc::new(DirectoryTaskStore::new(dataset_root.clone()));
    let provider_store = Arc::new(DirectoryProviderStore::new(dataset_root.clone()));
    let template_store = Arc::new(DirectoryTemplateStore::new(dataset_root.clone()));
    let dataset_writer = Arc::new(FilesystemDatasetWriter::new(dataset_root.clone()));

    let templates = setup_templates(&gc);
    let provider_configs = setup_providers(&gc, provider_store.as_ref());

    let mut providers = HashMap::new();
    for config in &provider_configs {
        let p = destilation_core::providers::create_provider(config.clone());
        providers.insert(config.id().clone(), Arc::from(p));
    }

    let validators = setup_validators(&gc);
    let metrics = Arc::new(InMemoryMetrics::new());
    let event_logger = Arc::new(BufferedFileEventLogger::new(2000, 500));

    let orchestrator = Orchestrator {
        job_store: job_store.clone(),
        task_store: task_store.clone(),
        provider_store: provider_store.clone(),
        providers: Arc::new(tokio::sync::RwLock::new(providers)),
        provider_configs: Arc::new(tokio::sync::RwLock::new(provider_configs)),
        template_store: template_store.clone(),
        templates: templates.clone(),
        validators,
        dataset_writer,
        metrics: metrics.clone(),
        logger: event_logger.clone(),
    };

    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(TuiUpdate::Templates(templates.values().cloned().collect()));
    
    spawn_monitor_tasks(tx, orchestrator.clone(), event_logger.clone());

    let (tx_cmd, rx_cmd) = std::sync::mpsc::channel();
    spawn_command_handler(rx_cmd, orchestrator.clone());

    // Auto-resume jobs that were already Running
    if let Ok(jobs) = job_store.list_jobs().await {
        for job in jobs {
            if job.status == destilation_core::domain::JobStatus::Running {
                let runner = orchestrator.clone();
                let job_id = job.id.clone();
                tokio::spawn(async move {
                    let _ = runner.run_job(&job_id).await;
                });
            }
        }
    }

    let metrics_clone = metrics.clone();
    let mut tui = Tui::new(metrics_clone, rx, tx_cmd, dataset_root.clone());
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_clone = done.clone();
    
    let tui_res = tokio::task::spawn_blocking(move || {
        tui.run(|| done_clone.load(std::sync::atomic::Ordering::Relaxed))
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

fn setup_templates(gc: &Option<GlobalConfig>) -> HashMap<TemplateId, TemplateConfig> {
    let mut templates = HashMap::new();
    if let Some(tconf) = gc.as_ref().and_then(|g| g.templates.as_ref()) {
        for t in tconf.values() {
            let id = t.id.clone().unwrap_or_else(|| "custom".to_string());
            let mode = match t.mode.as_deref() {
                Some("Simple") => TemplateMode::Simple,
                Some("Moe") => TemplateMode::Moe,
                Some("Reasoning") => TemplateMode::Reasoning,
                Some("Tools") => TemplateMode::Tools,
                Some("Quad") => TemplateMode::Quad,
                Some("Preference") => TemplateMode::Preference,
                Some("ToolTrace") => TemplateMode::ToolTrace,
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
        templates.insert(template.id.clone(), template);
    }
    templates
}

fn setup_providers(gc: &Option<GlobalConfig>, provider_store: &dyn ProviderStore) -> Vec<ProviderConfig> {
    let mut provider_configs = provider_store.list_providers().unwrap_or_default();
    if provider_configs.is_empty() {
        if let Some(pconf) = gc.as_ref().and_then(|g| g.providers.as_ref()) {
            if let Some(or) = &pconf.openrouter {
                if let Ok(key) = std::env::var(&or.api_key_env) {
                    let config = ProviderConfig::OpenRouter {
                        id: "openrouter".to_string(),
                        name: Some("OpenRouter".to_string()),
                        enabled: true,
                        base_url: or.base_url.clone(),
                        api_key: key,
                        model: or.model.clone(),
                    };
                    provider_configs.push(config.clone());
                    let _ = provider_store.save_provider(&config);
                }
            }
            if let Some(ol) = &pconf.ollama {
                let config = ProviderConfig::Ollama {
                    id: "ollama".to_string(),
                    name: Some("Ollama".to_string()),
                    enabled: true,
                    base_url: ol.base_url.clone(),
                    model: ol.model.clone(),
                };
                provider_configs.push(config.clone());
                let _ = provider_store.save_provider(&config);
            }
            if let Some(scripts) = &pconf.scripts {
                for (id, script_conf) in scripts {
                    let config = ProviderConfig::Script {
                        id: id.clone(),
                        name: Some(id.clone()),
                        enabled: true,
                        command: script_conf.command.clone(),
                        args: script_conf.args.clone().unwrap_or_default(),
                        timeout_ms: script_conf.timeout_ms,
                    };
                    provider_configs.push(config.clone());
                    let _ = provider_store.save_provider(&config);
                }
            }
        }
    }
    provider_configs
}

fn setup_validators(gc: &Option<GlobalConfig>) -> Vec<Arc<dyn Validator>> {
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
    validators
}

fn spawn_monitor_tasks(tx: std::sync::mpsc::Sender<TuiUpdate>, orchestrator: Orchestrator, logger: Arc<BufferedFileEventLogger>) {
    tokio::spawn(async move {
        let mut last_seq: u64 = 0;
        loop {
            if let Ok(jobs) = orchestrator.job_store.list_jobs().await {
                let _ = tx.send(TuiUpdate::Jobs(jobs.clone()));
                for job in jobs {
                    if let Ok(tasks) = orchestrator.task_store.list_tasks(&job.id).await {
                        let _ = tx.send(TuiUpdate::Tasks(job.id, tasks));
                    }
                }
            }
            let providers = orchestrator.list_providers().await;
            let _ = tx.send(TuiUpdate::Providers(providers));

            let (new_last, events) = logger.events_since(last_seq);
            last_seq = new_last;
            if !events.is_empty() {
                // Check for critical errors to show popup
                for event in &events {
                    if event.level == destilation_core::logging::LogLevel::Error && event.message == "job.error" {
                        if let Some(job_id) = &event.job_id {
                            if let Some(error_msg) = event.fields.get("error") {
                                let _ = tx.send(TuiUpdate::ErrorPopup(job_id.clone(), error_msg.clone()));
                            }
                        }
                    }
                }
                let _ = tx.send(TuiUpdate::LogEvents(events));
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });
}

fn spawn_command_handler(rx_cmd: std::sync::mpsc::Receiver<TuiCommand>, mut orchestrator: Orchestrator) {
    tokio::spawn(async move {
        loop {
            while let Ok(cmd) = rx_cmd.try_recv() {
                match cmd {
                    TuiCommand::PauseJob(id) => {
                        let _ = orchestrator.pause_job(&id).await;
                    }
                    TuiCommand::ResumeJob(id) => {
                        let prev_status = orchestrator
                            .job_store
                            .get_job(&id)
                            .await
                            .ok()
                            .flatten()
                            .map(|j| j.status.clone());
                        
                        let _ = orchestrator.resume_job(&id).await;
                        
                        // Spawn runner if it was Pending or if the loop could have been lost (e.g. status was Running but app restarted)
                        // Actually, to be safe, we should probably always spawn if it's not already running.
                        // Since we don't track active runners here, we'll assume ResumeJob needs a runner if it was Paused or Pending.
                        if matches!(prev_status, Some(destilation_core::domain::JobStatus::Pending) | Some(destilation_core::domain::JobStatus::Paused)) {
                            let runner = orchestrator.clone();
                            tokio::spawn(async move {
                                let _ = runner.run_job(&id).await;
                            });
                        }
                    }
                    TuiCommand::DeleteJob(id) => {
                        let _ = orchestrator.delete_job(&id).await;
                    }
                    TuiCommand::SaveProvider(config) => {
                        orchestrator.save_provider(config).await;
                    }
                    TuiCommand::DeleteProvider(id) => {
                        orchestrator.delete_provider(&id).await;
                    }
                    TuiCommand::SaveTemplate(template) => {
                        orchestrator.save_template(template).await;
                    }
                    TuiCommand::DeleteTemplate(id) => {
                        orchestrator.delete_template(&id).await;
                    }
                    TuiCommand::StartJob(config) => {
                        if let Ok(job) = orchestrator.submit_job(config).await {
                            let job_id = job.id.clone();
                            let runner = orchestrator.clone();
                            tokio::spawn(async move {
                                let _ = runner.run_job(&job_id).await;
                            });
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
}


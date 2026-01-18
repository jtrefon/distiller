use crate::domain::{Job, JobConfig, JobId, PromptSpec, Task, TaskState, TemplateConfig, TemplateId};
use crate::logging::{LogEvent, LogLevel, SharedEventLogger};
use crate::metrics::Metrics;
use crate::provider::ModelProvider;
use crate::storage::{TaskStore, TemplateStore};
use crate::validation::{ValidationContext, ValidationOutcome, Validator};
use rand::distributions::WeightedIndex;
use rand::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

pub async fn select_provider_id(
    config: &JobConfig,
    providers: &Arc<RwLock<HashMap<String, Arc<dyn ModelProvider>>>>,
    provider_configs: &Arc<RwLock<Vec<crate::provider::ProviderConfig>>>,
    logger: &SharedEventLogger,
) -> Option<String> {
    let mut candidates: Vec<(String, f32)> = Vec::new();
    let providers = providers.read().await;
    let configs = provider_configs.read().await;

    for spec in &config.providers {
        // Check if provider is enabled
        let is_enabled = configs
            .iter()
            .find(|c| c.id() == &spec.provider_id)
            .map(|c| c.is_enabled())
            .unwrap_or(false);

        if !is_enabled {
            continue;
        }

        if let Some(p) = providers.get(&spec.provider_id) {
            let meta = p.metadata();
            let required = &spec.capabilities_required;
            let ok = required.is_empty()
                || required
                    .iter()
                    .all(|r| meta.capabilities.iter().any(|c| c == r));
            if ok {
                candidates.push((spec.provider_id.clone(), spec.weight.max(0.0)));
            }
        }
    }
    if candidates.is_empty() {
        let fallback: Vec<(String, f32)> = configs
            .iter()
            .filter(|c| c.is_enabled())
            .filter_map(|c| {
                let id = c.id().clone();
                providers.get(&id).map(|_| (id, 1.0))
            })
            .collect();
        if fallback.is_empty() {
            return None;
        }
        logger.log(
            LogEvent::new(LogLevel::Warn, "providers.fallback")
                .with_field("configured_providers", config.providers.len().to_string())
                .with_field("fallback_candidates", fallback.len().to_string()),
        );
        candidates = fallback;
    }
    let weights: Vec<f32> = candidates.iter().map(|(_, w)| *w).collect();
    let dist = WeightedIndex::new(&weights).ok()?;
    let mut rng = thread_rng();
    let idx = dist.sample(&mut rng);
    Some(candidates[idx].0.clone())
}

pub async fn ensure_tasks_enqueued(
    job: &Job,
    task_store: &Arc<dyn TaskStore>,
    _template_store: &Arc<dyn TemplateStore>,
    templates: &HashMap<TemplateId, TemplateConfig>,
    providers: &Arc<RwLock<HashMap<String, Arc<dyn ModelProvider>>>>,
    provider_configs: &Arc<RwLock<Vec<crate::provider::ProviderConfig>>>,
    metrics: &Arc<dyn Metrics>,
    logger: &SharedEventLogger,
) -> anyhow::Result<()> {
    let existing_tasks = task_store.list_tasks(&job.id).await?;
    let mut existing_ids: HashSet<String> = existing_tasks.iter().map(|t| t.id.clone()).collect();

    let persisted_count = existing_tasks.iter().filter(|t| t.state == TaskState::Persisted).count() as u64;
    let pending_count = existing_tasks.iter().filter(|t| t.state == TaskState::Queued || t.state == TaskState::Generating || t.state == TaskState::Validating).count() as u64;

    let target = job.config.target_samples;
    let to_create = target.saturating_sub(persisted_count + pending_count);

    if to_create == 0 {
        return Ok(());
    }

    logger.log(
        LogEvent::new(LogLevel::Info, "tasks.enqueue")
            .with_job(job.id.clone())
            .with_field("count", to_create.to_string()),
    );

    let template = templates
        .get(&job.config.template_id)
        .ok_or_else(|| anyhow::anyhow!("template not found"))?
        .clone();

    let mut next_index: u64 = 1;
    for _ in 0..to_create {
        let provider_id = select_provider_id(&job.config, providers, provider_configs, logger).await.ok_or_else(|| {
            anyhow::anyhow!("no providers available for job")
        })?;

        let mut id = format!("task-{}-{}", job.id, next_index);
        while existing_ids.contains(&id) {
            next_index += 1;
            id = format!("task-{}-{}", job.id, next_index);
        }
        existing_ids.insert(id.clone());
        next_index += 1;

        let mut extra = HashMap::new();
        if let Some(domain) = job.config.domains.first() {
            extra.insert("domain".to_string(), domain.name.clone());
            // For 'distillation' jobs with {{prompt}}, we might want a variety of prompts.
            extra.insert("prompt".to_string(), format!("Provide a unique and complex reasoning task for {}", domain.name));
        }

        let task = Task {
            id,
            job_id: job.id.clone(),
            state: TaskState::Queued,
            attempts: 0,
            max_attempts: job.config.validation.max_attempts,
            provider_id: Some(provider_id),
            domain_id: job.config.domains.first().map(|d| d.id.clone()).unwrap_or_default(),
            template_id: job.config.template_id.clone(),
            prompt_spec: PromptSpec {
                system: Some(template.system_prompt.clone()),
                user: template.user_prompt_pattern.clone(),
                extra,
            },
            raw_response: None,
            validation_result: None,
            quality_score: None,
            is_negative: false,
        };

        task_store.enqueue_task(task).await?;
        metrics.inc_task_enqueued();
    }
    Ok(())
}

pub async fn count_persisted_tasks(
    task_store: &Arc<dyn TaskStore>,
    job_id: &JobId,
) -> u64 {
    task_store
        .list_tasks(job_id)
        .await
        .unwrap_or_default()
        .iter()
        .filter(|t| t.state == TaskState::Persisted)
        .count() as u64
}

pub fn run_validators(
    validators: &[Arc<dyn Validator>],
    ctx: &ValidationContext,
) -> ValidationOutcome {
    let mut passed = true;
    let mut issues = Vec::new();
    let mut score: Option<f32> = None;

    for v in validators {
        let o = v.validate(ctx);
        if !o.passed {
            passed = false;
        }
        if let Some(s) = o.score {
            score = Some(score.map_or(s, |x| x.min(s)));
        }
        if !o.issues.is_empty() {
            issues.extend(o.issues);
        }
    }

    ValidationOutcome {
        passed,
        issues,
        score,
    }
}
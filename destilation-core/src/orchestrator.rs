use crate::domain::{
    Job, JobConfig, JobId, PromptSpec, Sample, Task, TaskState, TemplateConfig, TemplateId,
};
use crate::logging::{LogEvent, LogLevel, SharedEventLogger};
use crate::metrics::Metrics;
use crate::provider::{GenerationRequest, ModelProvider, ProviderConfig};
use crate::storage::{DatasetWriter, JobStore, TaskStore};
use crate::validation::{ValidationContext, ValidationOutcome, Validator};
use rand::distributions::WeightedIndex;
use rand::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinSet;

#[derive(Clone)]
pub struct Orchestrator {
    pub job_store: Arc<dyn JobStore>,
    pub task_store: Arc<dyn TaskStore>,
    pub providers: Arc<RwLock<HashMap<String, Arc<dyn ModelProvider>>>>,
    pub provider_configs: Arc<RwLock<Vec<ProviderConfig>>>,
    pub templates: HashMap<TemplateId, TemplateConfig>,
    pub validators: Vec<Arc<dyn Validator>>,
    pub dataset_writer: Arc<dyn DatasetWriter>,
    pub metrics: Arc<dyn Metrics>,
    pub logger: SharedEventLogger,
}

impl Orchestrator {
    pub async fn submit_job(&self, config: JobConfig) -> anyhow::Result<Job> {
        let job = self.job_store.create_job(config).await?;
        self.metrics.inc_job_submitted();
        self.logger.log(
            LogEvent::new(LogLevel::Info, "job.submitted")
                .with_job(job.id.clone())
                .with_dataset_dir(job.config.output.dataset_dir.clone())
                .with_field("target_samples", job.config.target_samples.to_string())
                .with_field("max_concurrency", job.config.max_concurrency.to_string())
                .with_field("providers", job.config.providers.len().to_string()),
        );
        Ok(job)
    }

    pub async fn pause_job(&self, job_id: &JobId) -> anyhow::Result<()> {
        if let Some(mut job) = self.job_store.get_job(job_id).await? {
            job.status = crate::domain::JobStatus::Paused;
            self.job_store.update_job(&job).await?;
        }
        Ok(())
    }

    pub async fn resume_job(&self, job_id: &JobId) -> anyhow::Result<()> {
        if let Some(mut job) = self.job_store.get_job(job_id).await? {
            job.status = crate::domain::JobStatus::Running;
            self.job_store.update_job(&job).await?;
        }
        Ok(())
    }

    pub async fn delete_job(&self, job_id: &JobId) -> anyhow::Result<()> {
        // Delete tasks first to maintain referential integrity if enforced,
        // though our trait methods handle them independently.
        self.task_store.delete_tasks_by_job(job_id).await?;
        self.job_store.delete_job(job_id).await?;
        Ok(())
    }

    pub async fn clean_database(&self) -> anyhow::Result<()> {
        let jobs = self.job_store.list_jobs().await?;
        for job in jobs {
            self.delete_job(&job.id).await?;
        }
        Ok(())
    }

    pub async fn select_provider_id(&self, config: &JobConfig) -> Option<String> {
        let mut candidates: Vec<(String, f32)> = Vec::new();
        let providers = self.providers.read().await;
        let configs = self.provider_configs.read().await;

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
            return None;
        }
        let weights: Vec<f32> = candidates.iter().map(|(_, w)| *w).collect();
        let dist = WeightedIndex::new(&weights).ok()?;
        let mut rng = thread_rng();
        let idx = dist.sample(&mut rng);
        Some(candidates[idx].0.clone())
    }

    pub async fn run_job(&self, job_id: &JobId) -> anyhow::Result<()> {
        let mut job = self
            .job_store
            .get_job(job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job not found"))?;

        let target = job.config.target_samples;

        job.status = crate::domain::JobStatus::Running;
        if job.started_at.is_none() {
            job.started_at = Some(chrono::Utc::now());
        }
        job.updated_at = chrono::Utc::now();
        job.finished_at = None;
        self.job_store.update_job(&job).await?;
        self.logger.log(
            LogEvent::new(LogLevel::Info, "job.started")
                .with_job(job.id.clone())
                .with_dataset_dir(job.config.output.dataset_dir.clone())
                .with_field("target_samples", target.to_string())
                .with_field("max_concurrency", job.config.max_concurrency.to_string()),
        );

        let run_res: anyhow::Result<(u64, Option<anyhow::Error>)> = async {
            let existing_tasks = self.task_store.list_tasks(job_id).await?;
            let mut reset_inflight: u64 = 0;
            for mut t in existing_tasks.iter().cloned() {
                if matches!(t.state, TaskState::Generating | TaskState::Validating) {
                    t.state = TaskState::Queued;
                    self.task_store.update_task(&t).await?;
                    reset_inflight += 1;
                }
            }
            if reset_inflight > 0 {
                self.logger.log(
                    LogEvent::new(LogLevel::Warn, "tasks.reset_inflight")
                        .with_job(job.id.clone())
                        .with_dataset_dir(job.config.output.dataset_dir.clone())
                        .with_field("count", reset_inflight.to_string()),
                );
            }

            let existing_tasks = self.task_store.list_tasks(job_id).await?;
            let mut existing_ids: HashSet<String> =
                existing_tasks.iter().map(|t| t.id.clone()).collect();
            let mut next_index: u64 = 1;
            while existing_ids.contains(&format!("task-{}-{}", job.id, next_index)) {
                next_index += 1;
            }

            let to_create = target.saturating_sub(existing_tasks.len() as u64);
            if to_create > 0 {
                self.logger.log(
                    LogEvent::new(LogLevel::Info, "tasks.enqueue")
                        .with_job(job.id.clone())
                        .with_dataset_dir(job.config.output.dataset_dir.clone())
                        .with_field("count", to_create.to_string()),
                );
            }
            for _ in 0..to_create {
                let template = self
                    .templates
                    .get(&job.config.template_id)
                    .ok_or_else(|| anyhow::anyhow!("template not found"))?
                    .clone();

                let provider_id = self.select_provider_id(&job.config).await.ok_or_else(|| {
                    self.logger.log(
                        LogEvent::new(LogLevel::Error, "job.no_providers_available")
                            .with_job(job.id.clone())
                            .with_dataset_dir(job.config.output.dataset_dir.clone()),
                    );
                    anyhow::anyhow!("no providers available for job")
                })?;

                let prompt = PromptSpec {
                    system: Some(template.system_prompt.clone()),
                    user: template.user_prompt_pattern.clone(),
                    extra: HashMap::new(),
                };

                let mut id = format!("task-{}-{}", job.id, next_index);
                while existing_ids.contains(&id) {
                    next_index += 1;
                    id = format!("task-{}-{}", job.id, next_index);
                }
                existing_ids.insert(id.clone());
                next_index += 1;

                let task = Task {
                    id,
                    job_id: job.id.clone(),
                    state: TaskState::Queued,
                    attempts: 0,
                    max_attempts: job.config.validation.max_attempts,
                    provider_id: Some(provider_id),
                    domain_id: job
                        .config
                        .domains
                        .first()
                        .map(|d| d.id.clone())
                        .unwrap_or_else(|| "default".to_string()),
                    template_id: job.config.template_id.clone(),
                    prompt_spec: prompt,
                    raw_response: None,
                    validation_result: None,
                };

                self.task_store.enqueue_task(task).await?;
                self.metrics.inc_task_enqueued();
            }

            let mut join_set: JoinSet<anyhow::Result<()>> = JoinSet::new();
            let max_concurrency = job.config.max_concurrency.max(1) as usize;

            let mut run_err: Option<anyhow::Error> = None;
            loop {
                if let Some(j) = self.job_store.get_job(job_id).await? {
                    if j.status == crate::domain::JobStatus::Paused {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                }

                while join_set.len() < max_concurrency {
                    let tasks = self.task_store.list_tasks(job_id).await?;
                    let next = tasks.into_iter().find(|t| t.state == TaskState::Queued);
                    let Some(mut task) = next else { break };

                    task.state = TaskState::Generating;
                    self.task_store.update_task(&task).await?;

                    let this = self.clone();
                    let job_clone = job.clone();
                    join_set.spawn(async move { this.process_task(job_clone, task).await });
                }

                if join_set.is_empty() {
                    let tasks = self.task_store.list_tasks(job_id).await?;
                    let has_more = tasks.iter().any(|t| t.state == TaskState::Queued);
                    if has_more {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                    break;
                }

                if let Some(res) = join_set.join_next().await {
                    match res {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            run_err = Some(e);
                            break;
                        }
                        Err(e) => {
                            run_err = Some(anyhow::anyhow!(e));
                            break;
                        }
                    }
                }
            }

            let tasks = self.task_store.list_tasks(job_id).await?;
            let saved = tasks
                .iter()
                .filter(|t| t.state == TaskState::Persisted)
                .count() as u64;

            Ok((saved, run_err))
        }
        .await;

        let (saved, fatal_err) = match run_res {
            Ok((saved, err)) => (saved, err),
            Err(e) => {
                let saved = match self.task_store.list_tasks(job_id).await {
                    Ok(tasks) => tasks
                        .iter()
                        .filter(|t| t.state == TaskState::Persisted)
                        .count() as u64,
                    Err(_) => 0,
                };
                (saved, Some(e))
            }
        };

        job.completed_samples = saved;
        job.updated_at = chrono::Utc::now();
        job.finished_at = Some(chrono::Utc::now());

        if fatal_err.is_none() && saved >= target {
            job.status = crate::domain::JobStatus::Completed;
        } else {
            job.status = crate::domain::JobStatus::Failed;
        }
        self.job_store.update_job(&job).await?;
        self.logger.log(
            LogEvent::new(LogLevel::Info, "job.finished")
                .with_job(job.id.clone())
                .with_dataset_dir(job.config.output.dataset_dir.clone())
                .with_field("status", format!("{:?}", job.status))
                .with_field("saved", saved.to_string())
                .with_field("target", target.to_string()),
        );

        if let Some(e) = fatal_err {
            self.logger.log(
                LogEvent::new(LogLevel::Error, "job.error")
                    .with_job(job.id.clone())
                    .with_dataset_dir(job.config.output.dataset_dir.clone())
                    .with_field("error", format!("{:?}", e)),
            );
            return Err(e);
        }

        Ok(())
    }

    async fn process_task(&self, job: Job, mut task: Task) -> anyhow::Result<()> {
        self.metrics.inc_task_started();
        self.logger.log(
            LogEvent::new(LogLevel::Info, "task.started")
                .with_job(job.id.clone())
                .with_task(task.id.clone())
                .with_dataset_dir(job.config.output.dataset_dir.clone())
                .with_field("attempt", task.attempts.to_string())
                .with_field("max_attempts", task.max_attempts.to_string()),
        );

        let provider_id = if let Some(id) = task.provider_id.clone() {
            id
        } else {
            self.select_provider_id(&job.config).await.ok_or_else(|| {
                self.logger.log(
                    LogEvent::new(LogLevel::Error, "task.no_providers_available")
                        .with_job(job.id.clone())
                        .with_task(task.id.clone())
                        .with_dataset_dir(job.config.output.dataset_dir.clone()),
                );
                anyhow::anyhow!("no providers available for job")
            })?
        };
        task.provider_id = Some(provider_id.clone());

        task.state = TaskState::Generating;
        self.task_store.update_task(&task).await?;

        let provider = {
            let providers = self.providers.read().await;
            providers
                .get(&provider_id)
                .ok_or_else(|| anyhow::anyhow!("provider not registered"))?
                .clone()
        };

        let template = self
            .templates
            .get(&task.template_id)
            .ok_or_else(|| anyhow::anyhow!("template not found"))?
            .clone();

        let model_name = {
            let configs = self.provider_configs.read().await;
            configs
                .iter()
                .find(|c| c.id() == &provider_id)
                .map(|c| match c {
                    ProviderConfig::OpenRouter { model, .. } => model.clone(),
                    ProviderConfig::Ollama { model, .. } => model.clone(),
                    ProviderConfig::Script { .. } => "script".to_string(),
                })
                .unwrap_or_else(|| "unknown".to_string())
        };

        let req = GenerationRequest {
            provider_id: provider_id.clone(),
            model: model_name,
            prompt: task.prompt_spec.clone(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            metadata: HashMap::new(),
        };

        self.logger.log(
            LogEvent::new(LogLevel::Debug, "task.generate.request")
                .with_job(job.id.clone())
                .with_task(task.id.clone())
                .with_dataset_dir(job.config.output.dataset_dir.clone())
                .with_field("provider_id", provider_id.clone())
                .with_field("model", req.model.clone()),
        );

        let res = match provider.generate(req).await {
            Ok(r) => r,
            Err(e) => {
                self.logger.log(
                    LogEvent::new(LogLevel::Error, "task.generate.error")
                        .with_job(job.id.clone())
                        .with_task(task.id.clone())
                        .with_dataset_dir(job.config.output.dataset_dir.clone())
                        .with_field("provider_id", provider_id.clone())
                        .with_field("error", format!("{:?}", e)),
                );
                let mut issue_details = HashMap::new();
                issue_details.insert("provider_id".to_string(), provider_id.clone());
                issue_details.insert("error".to_string(), format!("{:?}", e));

                task.validation_result = Some(ValidationOutcome {
                    passed: false,
                    issues: vec![crate::validation::ValidationIssue {
                        code: "provider.generate".to_string(),
                        message: "Provider generation failed".to_string(),
                        severity: crate::validation::ValidationSeverity::Error,
                        details: issue_details,
                    }],
                    score: None,
                });
                self.metrics.record_validator_fail();

                task.attempts += 1;
                if task.attempts >= task.max_attempts {
                    task.state = TaskState::Rejected;
                    self.metrics.inc_task_rejected();
                } else {
                    task.state = TaskState::Queued;
                }
                self.task_store.update_task(&task).await?;
                return Ok(());
            }
        };

        self.logger.log(
            LogEvent::new(LogLevel::Debug, "task.generate.ok")
                .with_job(job.id.clone())
                .with_task(task.id.clone())
                .with_dataset_dir(job.config.output.dataset_dir.clone())
                .with_field("latency_ms", res.latency.as_millis().to_string())
                .with_field("raw_len", res.raw_output.len().to_string()),
        );

        task.raw_response = Some(res.raw_output.clone());

        task.state = TaskState::Validating;
        self.task_store.update_task(&task).await?;

        let parsed = serde_json::from_str::<serde_json::Value>(&res.raw_output).ok();

        let ctx = ValidationContext {
            job,
            task: task.clone(),
            template,
            provider_result: res.clone(),
            parsed_output: parsed.clone(),
        };

        let outcome = self.run_validators(&ctx);
        task.validation_result = Some(outcome.clone());
        if outcome.passed {
            self.metrics.record_validator_pass();
        } else {
            self.metrics.record_validator_fail();
        }
        self.logger.log(
            LogEvent::new(
                if outcome.passed {
                    LogLevel::Info
                } else {
                    LogLevel::Warn
                },
                "task.validate",
            )
            .with_job(ctx.job.id.clone())
            .with_task(task.id.clone())
            .with_dataset_dir(ctx.job.config.output.dataset_dir.clone())
            .with_field("passed", outcome.passed.to_string())
            .with_field("issues", outcome.issues.len().to_string()),
        );

        if outcome.passed {
            let sample = Sample {
                id: format!("sample-{}", task.id),
                job_id: task.job_id.clone(),
                task_id: task.id.clone(),
                provider_id: provider_id.clone(),
                model_name: res.model.clone(),
                template_id: task.template_id.clone(),
                schema_version: "v1".to_string(),
                payload: parsed.unwrap_or(serde_json::json!({"raw": res.raw_output})),
                quality_score: outcome.score,
                tags: vec![],
            };
            self.dataset_writer.persist_sample(sample).await?;
            self.metrics.inc_samples_persisted();
            task.state = TaskState::Persisted;
            self.metrics.inc_task_persisted();
            self.logger.log(
                LogEvent::new(LogLevel::Info, "task.persisted")
                    .with_job(ctx.job.id.clone())
                    .with_task(task.id.clone())
                    .with_dataset_dir(ctx.job.config.output.dataset_dir.clone())
                    .with_field("provider_id", provider_id.clone())
                    .with_field("model", res.model.clone()),
            );
        } else {
            task.attempts += 1;
            if task.attempts >= task.max_attempts {
                task.state = TaskState::Rejected;
                self.metrics.inc_task_rejected();
                self.logger.log(
                    LogEvent::new(LogLevel::Warn, "task.rejected")
                        .with_job(ctx.job.id.clone())
                        .with_task(task.id.clone())
                        .with_dataset_dir(ctx.job.config.output.dataset_dir.clone())
                        .with_field("attempts", task.attempts.to_string()),
                );
            } else {
                task.state = TaskState::Queued;
                self.logger.log(
                    LogEvent::new(LogLevel::Debug, "task.requeued")
                        .with_job(ctx.job.id.clone())
                        .with_task(task.id.clone())
                        .with_dataset_dir(ctx.job.config.output.dataset_dir.clone())
                        .with_field("attempts", task.attempts.to_string()),
                );
            }
        }

        self.task_store.update_task(&task).await?;
        Ok(())
    }

    fn run_validators(&self, ctx: &ValidationContext) -> ValidationOutcome {
        let mut passed = true;
        let mut issues = Vec::new();
        let mut score: Option<f32> = None;

        for v in &self.validators {
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

    pub async fn save_provider(&self, config: ProviderConfig) {
        use crate::providers::create_provider;
        let id = config.id().clone();
        let provider = create_provider(config.clone());

        // Update both maps
        {
            let mut providers = self.providers.write().await;
            providers.insert(id.clone(), Arc::from(provider));
        }
        {
            let mut configs = self.provider_configs.write().await;
            // Replace if exists, or push
            if let Some(pos) = configs.iter().position(|c| c.id() == &id) {
                configs[pos] = config;
            } else {
                configs.push(config);
            }
        }
    }

    pub async fn delete_provider(&self, id: &str) {
        {
            let mut providers = self.providers.write().await;
            providers.remove(id);
        }
        {
            let mut configs = self.provider_configs.write().await;
            if let Some(pos) = configs.iter().position(|c| c.id() == id) {
                configs.remove(pos);
            }
        }
    }

    pub async fn list_providers(&self) -> Vec<ProviderConfig> {
        let configs = self.provider_configs.read().await;
        configs.clone()
    }
}

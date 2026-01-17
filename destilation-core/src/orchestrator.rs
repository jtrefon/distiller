use crate::domain::{
    Job, JobConfig, JobId, PromptSpec, Sample, Task, TaskId, TaskState, TemplateConfig, TemplateId,
    TemplateMode,
};
use crate::logging::{LogEvent, LogLevel, SharedEventLogger};
use crate::metrics::Metrics;
use crate::provider::{GenerationRequest, ModelProvider, ProviderConfig};
use crate::storage::{DatasetWriter, JobStore, ProviderStore, TaskStore, TemplateStore};
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
    pub provider_store: Arc<dyn ProviderStore>,
    pub providers: Arc<RwLock<HashMap<String, Arc<dyn ModelProvider>>>>,
    pub provider_configs: Arc<RwLock<Vec<ProviderConfig>>>,
    pub template_store: Arc<dyn TemplateStore>,
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
        let mut job = self.prepare_job(job_id).await?;
        let target = job.config.target_samples;

        self.logger.log(
            LogEvent::new(LogLevel::Info, "job.started")
                .with_job(job.id.clone())
                .with_dataset_dir(job.config.output.dataset_dir.clone())
                .with_field("target_samples", target.to_string())
                .with_field("max_concurrency", job.config.max_concurrency.to_string()),
        );

        let run_res = self.execute_job_logic(&mut job).await;

        let (saved, fatal_err) = match run_res {
            Ok(saved) => (saved, None),
            Err(e) => {
                let saved = self.count_persisted_tasks(&job.id).await;
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

    async fn prepare_job(&self, job_id: &JobId) -> anyhow::Result<Job> {
        let mut job = self
            .job_store
            .get_job(job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job not found"))?;

        job.status = crate::domain::JobStatus::Running;
        if job.started_at.is_none() {
            job.started_at = Some(chrono::Utc::now());
        }
        job.updated_at = chrono::Utc::now();
        job.finished_at = None;
        self.job_store.update_job(&job).await?;
        Ok(job)
    }

    async fn execute_job_logic(&self, job: &Job) -> anyhow::Result<u64> {
        self.reset_inflight_tasks(&job.id).await?;
        self.ensure_tasks_enqueued(job).await?;

        let mut join_set: JoinSet<anyhow::Result<TaskId>> = JoinSet::new();
        let max_concurrency = job.config.max_concurrency.max(1) as usize;
        let mut in_flight_ids = HashSet::new();

        loop {
            // Check if paused
            if let Some(j) = self.job_store.get_job(&job.id).await? {
                if j.status == crate::domain::JobStatus::Paused {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    continue;
                }
            }

            // Periodic heartbeat
            self.logger.log(
                LogEvent::new(LogLevel::Debug, "orchestrator.heartbeat")
                    .with_job(job.id.clone())
                    .with_dataset_dir(job.config.output.dataset_dir.clone())
                    .with_field("sentinel", "v4-replenish".to_string()),
            );

            // Periodically ensure we have enough tasks enqueued (handles replacements)
            let _ = self.ensure_tasks_enqueued(job).await;

            // Refill local queue if needed
            if join_set.len() < max_concurrency {
                let tasks = self.task_store.list_tasks(&job.id).await?;
                for mut task in tasks {
                    if task.state == TaskState::Queued && !in_flight_ids.contains(&task.id) {
                        task.state = TaskState::Generating;
                        self.task_store.update_task(&task).await?;

                        self.logger.log(
                            LogEvent::new(LogLevel::Trace, "orchestrator.loop.spawn_task")
                                .with_job(job.id.clone())
                                .with_task(task.id.clone()),
                        );
                        
                        in_flight_ids.insert(task.id.clone());
                        let this = self.clone();
                        let job_clone = job.clone();
                        join_set.spawn(async move { this.process_task(job_clone, task).await });

                        if join_set.len() >= max_concurrency {
                            break;
                        }
                    }
                }
            }

            if join_set.is_empty() {
                let tasks = self.task_store.list_tasks(&job.id).await?;
                let has_more = tasks.iter().any(|t| t.state == TaskState::Queued);
                if has_more {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                break;
            }

            if let Some(res) = join_set.join_next().await {
                match res {
                    Ok(Ok(task_id)) => {
                        in_flight_ids.remove(&task_id);
                    }
                    Ok(Err(e)) => {
                        // An error occurred during task processing.
                        // We don't know which task failed here without returning the ID from the error.
                        // For now, we'll clear in_flight_ids and let the next loop iteration re-evaluate.
                        // If the error is critical, we might want to propagate it.
                        self.logger.log(
                            LogEvent::new(LogLevel::Error, "orchestrator.task_error")
                                .with_job(job.id.clone())
                                .with_field("error", format!("{:?}", e)),
                        );
                        in_flight_ids.clear();
                    }
                    Err(e) => {
                        // JoinError, likely from task cancellation or panic.
                        self.logger.log(
                            LogEvent::new(LogLevel::Error, "orchestrator.join_error")
                                .with_job(job.id.clone())
                                .with_field("error", format!("{:?}", e)),
                        );
                        in_flight_ids.clear();
                    }
                }
            }
        }

        Ok(self.count_persisted_tasks(&job.id).await)
    }

    fn format_prompt(&self, pattern: &str, extra: &HashMap<String, String>) -> String {
        let mut out = pattern.to_string();
        for (k, v) in extra {
            out = out.replace(&format!("{{{{{}}}}}", k), v);
        }
        
        // Secondary pass for {{domain}} if not explicitly in extra but available as a fallback
        if out.contains("{{domain}}") {
            if let Some(d) = extra.get("domain") {
                out = out.replace("{{domain}}", d);
            } else {
                out = out.replace("{{domain}}", "General");
            }
        }

        // Tertiary pass for {{prompt}} fallback
        if out.contains("{{prompt}}") {
            if let Some(p) = extra.get("prompt") {
                out = out.replace("{{prompt}}", p);
            } else {
                let dom = extra.get("domain").map(|s| s.as_str()).unwrap_or("General");
                out = out.replace("{{prompt}}", &format!("Generate a high-quality distillation sample for the {} domain", dom));
            }
        }
        out
    }

    async fn reset_inflight_tasks(&self, job_id: &JobId) -> anyhow::Result<()> {
        let existing_tasks = self.task_store.list_tasks(job_id).await?;
        let mut reset_count = 0;
        for mut t in existing_tasks {
            if matches!(t.state, TaskState::Generating | TaskState::Validating) {
                t.state = TaskState::Queued;
                self.task_store.update_task(&t).await?;
                reset_count += 1;
            }
        }
        if reset_count > 0 {
            self.logger.log(
                LogEvent::new(LogLevel::Warn, "tasks.reset_inflight")
                    .with_job(job_id.clone())
                    .with_field("count", reset_count.to_string()),
            );
        }
        Ok(())
    }

    async fn ensure_tasks_enqueued(&self, job: &Job) -> anyhow::Result<()> {
        let existing_tasks = self.task_store.list_tasks(&job.id).await?;
        let mut existing_ids: HashSet<String> = existing_tasks.iter().map(|t| t.id.clone()).collect();
        
        let persisted_count = existing_tasks.iter().filter(|t| t.state == TaskState::Persisted).count() as u64;
        let pending_count = existing_tasks.iter().filter(|t| t.state == TaskState::Queued || t.state == TaskState::Generating || t.state == TaskState::Validating).count() as u64;

        let target = job.config.target_samples;
        let to_create = target.saturating_sub(persisted_count + pending_count);

        if to_create == 0 {
            return Ok(());
        }

        self.logger.log(
            LogEvent::new(LogLevel::Info, "tasks.enqueue")
                .with_job(job.id.clone())
                .with_field("count", to_create.to_string()),
        );

        let template = self
            .templates
            .get(&job.config.template_id)
            .ok_or_else(|| anyhow::anyhow!("template not found"))?
            .clone();

        let mut next_index: u64 = 1;
        for _ in 0..to_create {
            let provider_id = self.select_provider_id(&job.config).await.ok_or_else(|| {
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

            self.task_store.enqueue_task(task).await?;
            self.metrics.inc_task_enqueued();
        }
        Ok(())
    }

    async fn count_persisted_tasks(&self, job_id: &JobId) -> u64 {
        self.task_store
            .list_tasks(job_id)
            .await
            .unwrap_or_default()
            .iter()
            .filter(|t| t.state == TaskState::Persisted)
            .count() as u64
    }

    async fn process_task(&self, job: Job, mut task: Task) -> anyhow::Result<TaskId> {
        let task_id = task.id.clone();
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

        // Format prompts
        let mut system_prompt = template.system_prompt.clone();
        
        // Mode-specific instructions
        match template.mode {
            TemplateMode::Quad => {
                system_prompt += "\n\nThis is a QUAD setup. Your response MUST include: 'context' (background info), 'question' (specific problem), 'thought' (step-by-step reasoning), and 'answer' (final result).";
            }
            TemplateMode::Preference => {
                system_prompt += "\n\nThis is a PREFERENCE setup. You should provide a high-quality 'chosen' reasoning/answer pair AND a subtly flawed or suboptimal 'rejected' version.";
            }
            TemplateMode::ToolTrace => {
                system_prompt += "\n\nThis is a TOOL-TRACE setup. Explicitly document any tool calls and their impacts on the reasoning process.";
            }
            _ => {}
        }

        // Inject schema instructions if not present
        if !system_prompt.contains("JSON") {
            let fields: Vec<String> = template.schema.fields.iter().map(|f| format!("'{}' ({})", f.name, f.field_type)).collect();
            system_prompt += &format!("\n\nYour response MUST be a valid JSON object with the following fields: {}. Important: Do not include any conversational text outside the JSON block.", fields.join(", "));
        }

        let system_prompt_formatted = self.format_prompt(&system_prompt, &task.prompt_spec.extra);
        let user_prompt_formatted = self.format_prompt(&task.prompt_spec.user, &task.prompt_spec.extra);

        self.logger.log(
            LogEvent::new(LogLevel::Trace, "orchestrator.prompt.formatted")
                .with_job(job.id.clone())
                .with_task(task.id.clone())
                .with_field("user_prompt", user_prompt_formatted.clone()),
        );

        let req = GenerationRequest {
            provider_id: provider_id.clone(),
            model: model_name,
            prompt: PromptSpec {
                system: Some(system_prompt_formatted),
                user: user_prompt_formatted,
                extra: task.prompt_spec.extra.clone(),
            },
            max_tokens: None,
            temperature: None,
            top_p: None,
            metadata: HashMap::new(),
        };

        self.logger.log(
            LogEvent::new(LogLevel::Debug, "task.generate.submit")
                .with_job(job.id.clone())
                .with_task(task.id.clone())
                .with_field("provider_id", provider_id.clone())
                .with_field("model", req.model.clone()),
        );

        let res = match tokio::time::timeout(std::time::Duration::from_secs(60), provider.generate(req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
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
                
                let is_critical = matches!(e, crate::provider::ProviderError::Critical(_));
                if let crate::provider::ProviderError::Critical(msg) = &e {
                    // Pause job immediately
                    if let Err(err) = self.pause_job(&job.id).await {
                        self.logger.log(
                            LogEvent::new(LogLevel::Error, "job.pause_failed")
                                .with_job(job.id.clone())
                                .with_field("error", err.to_string()),
                        );
                    }
                    
                    self.logger.log(
                        LogEvent::new(LogLevel::Error, "job.error")
                            .with_job(job.id.clone())
                            .with_dataset_dir(job.config.output.dataset_dir.clone())
                            .with_field("error", msg.clone()),
                    );
                    
                    // Don't requeue task immediately, let resume handle it
                    // But we need to update state so it's not lost
                    task.state = TaskState::Queued;
                    self.task_store.update_task(&task).await?;
                    return Ok(task_id);
                }

                let is_rate_limit = matches!(e, crate::provider::ProviderError::RateLimited);
                if is_rate_limit {
                    task.attempts = task.attempts.saturating_sub(1);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }

                if task.attempts >= task.max_attempts {
                    task.state = TaskState::Rejected;
                    self.metrics.inc_task_rejected();
                } else {
                    task.state = TaskState::Queued;
                }
                self.task_store.update_task(&task).await?;
                return Ok(task_id);
            }
            Err(_) => {
                self.logger.log(
                    LogEvent::new(LogLevel::Error, "task.generate.timeout")
                        .with_job(job.id.clone())
                        .with_task(task.id.clone())
                        .with_dataset_dir(job.config.output.dataset_dir.clone())
                        .with_field("provider_id", provider_id.clone()),
                );
                let mut issue_details = HashMap::new();
                issue_details.insert("provider_id".to_string(), provider_id.clone());
                issue_details.insert("error".to_string(), "Generation timed out after 60s".to_string());

                task.validation_result = Some(ValidationOutcome {
                    passed: false,
                    issues: vec![crate::validation::ValidationIssue {
                        code: "provider.timeout".to_string(),
                        message: "Provider generation timed out".to_string(),
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
                return Ok(task_id);
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

        let raw = res.raw_output.trim();
        self.logger.log(
            LogEvent::new(LogLevel::Trace, "task.parse.start")
                .with_job(job.id.clone())
                .with_task(task.id.clone()),
        );

        let json_str = if raw.starts_with('{') {
            raw.to_string()
        } else {
            // Try to extract JSON from markdown/text
            if let Some(start) = raw.find('{') {
                if let Some(end) = raw.rfind('}') {
                    raw[start..=end].to_string()
                } else {
                    raw.to_string()
                }
            } else {
                raw.to_string()
            }
        };

        let parsed = serde_json::from_str::<serde_json::Value>(&json_str).ok();

        let ctx = ValidationContext {
            job,
            task: task.clone(),
            template,
            provider_result: res.clone(),
            parsed_output: parsed.clone(),
        };

        self.logger.log(
            LogEvent::new(LogLevel::Trace, "task.validate.start")
                .with_job(ctx.job.id.clone())
                .with_task(task.id.clone()),
        );

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
                is_negative: task.is_negative,
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
        Ok(task_id)
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

        // Persist
        if let Err(e) = self.provider_store.save_provider(&config) {
            self.logger.log(
                LogEvent::new(LogLevel::Error, "orchestrator.provider.save_error")
                    .with_field("provider_id", id.clone())
                    .with_field("error", e.to_string()),
            );
        }

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
        // Persist
        if let Err(e) = self.provider_store.delete_provider(&id.to_string()) {
            self.logger.log(
                LogEvent::new(LogLevel::Error, "orchestrator.provider.delete_error")
                    .with_field("provider_id", id.to_string())
                    .with_field("error", e.to_string()),
            );
        }

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

    pub async fn save_template(&mut self, template: TemplateConfig) {
        let id = template.id.clone();
        if let Err(e) = self.template_store.save_template(&template) {
            self.logger.log(
                LogEvent::new(LogLevel::Error, "orchestrator.template.save_error")
                    .with_field("template_id", id.clone())
                    .with_field("error", e.to_string()),
            );
        }
        self.templates.insert(id, template);
    }

    pub async fn delete_template(&mut self, id: &TemplateId) {
        if let Err(e) = self.template_store.delete_template(id) {
            self.logger.log(
                LogEvent::new(LogLevel::Error, "orchestrator.template.delete_error")
                    .with_field("template_id", id.clone())
                    .with_field("error", e.to_string()),
            );
        }
        self.templates.remove(id);
    }

    pub fn list_templates(&self) -> Vec<TemplateConfig> {
        self.templates.values().cloned().collect()
    }
}

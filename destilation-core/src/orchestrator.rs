use crate::domain::{
    Job, JobConfig, JobId, PromptSpec, Sample, Task, TaskId, TaskState, TemplateConfig, TemplateId,
    TemplateMode,
};
use crate::logging::{LogEvent, LogLevel, SharedEventLogger};
use crate::metrics::Metrics;
use crate::provider::{GenerationRequest, GenerationResult, ModelProvider, ProviderConfig};
use crate::shared;
use crate::storage::{DatasetWriter, JobStore, ProviderStore, TaskStore, TemplateStore};
use crate::validation::{ValidationContext, ValidationOutcome, Validator};
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

        // Create and start the processor
        let processor = crate::processor::DatasetProcessor::new(
            self.job_store.clone(),
            self.task_store.clone(),
            self.provider_store.clone(),
            self.providers.clone(),
            self.provider_configs.clone(),
            self.template_store.clone(),
            self.templates.clone(),
            self.validators.clone(),
            self.dataset_writer.clone(),
            self.metrics.clone(),
            self.logger.clone(),
        );

        let run_res = processor.process_job(job_id.clone()).await;

        let (saved, fatal_err) = match run_res {
            Ok(saved) => (saved, None),
            Err(e) => {
                let saved = shared::count_persisted_tasks(&self.task_store, &job.id).await;
                (saved, Some(e))
            }
        };

        job.completed_samples = saved;
        job.updated_at = chrono::Utc::now();
        job.finished_at = Some(chrono::Utc::now());

        if let Some(_) = &fatal_err {
            // If a fatal error occurred, preserve a PAUSED state (set by components like processor)
            // otherwise mark as Failed.
            if let Some(j) = self.job_store.get_job(&job.id).await? {
                if j.status == crate::domain::JobStatus::Paused {
                    job.status = crate::domain::JobStatus::Paused;
                } else {
                    job.status = crate::domain::JobStatus::Failed;
                }
            } else {
                job.status = crate::domain::JobStatus::Failed;
            }
        } else if saved >= target {
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
        shared::ensure_tasks_enqueued(job, &self.task_store, &self.template_store, &self.templates, &self.providers, &self.provider_configs, &self.metrics, &self.logger).await?;

        let mut join_set: JoinSet<(TaskId, anyhow::Result<TaskId>)> = JoinSet::new();
        let max_concurrency = job.config.max_concurrency.max(1) as usize;
        let mut in_flight_ids = HashSet::new();

        loop {
            self.logger.log(
                LogEvent::new(LogLevel::Debug, "orchestrator.loop.iteration")
                    .with_job(job.id.clone())
                    .with_field("join_set_len", join_set.len().to_string())
                    .with_field("in_flight_count", in_flight_ids.len().to_string()),
            );

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
            shared::ensure_tasks_enqueued(job, &self.task_store, &self.template_store, &self.templates, &self.providers, &self.provider_configs, &self.metrics, &self.logger).await?;

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
                        join_set.spawn(async move { (task.id.clone(), this.process_task(job_clone, task).await) });

                        if join_set.len() >= max_concurrency {
                            break;
                        }
                    }
                }
            }

            if join_set.is_empty() {
                let tasks = self.task_store.list_tasks(&job.id).await?;
                let has_more = tasks.iter().any(|t| t.state == TaskState::Queued);
                let persisted_count = tasks.iter().filter(|t| t.state == TaskState::Persisted).count();
                let rejected_count = tasks.iter().filter(|t| t.state == TaskState::Rejected).count();
                self.logger.log(
                    LogEvent::new(LogLevel::Debug, "orchestrator.loop.check_empty")
                        .with_job(job.id.clone())
                        .with_field("has_more", has_more.to_string())
                        .with_field("persisted", persisted_count.to_string())
                        .with_field("rejected", rejected_count.to_string())
                        .with_field("total_tasks", tasks.len().to_string()),
                );
                if has_more {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                self.logger.log(
                    LogEvent::new(LogLevel::Info, "orchestrator.loop.break")
                        .with_job(job.id.clone()),
                );
                break;
            }

            if let Some(res) = join_set.join_next().await {
                match res {
                    Ok((task_id, Ok(_))) => {
                        in_flight_ids.remove(&task_id);
                    }
                    Ok((task_id, Err(e))) => {
                        in_flight_ids.remove(&task_id);
                        self.logger.log(
                            LogEvent::new(LogLevel::Error, "orchestrator.task_error")
                                .with_job(job.id.clone())
                                .with_task(task_id)
                                .with_field("error", format!("{:?}", e)),
                        );
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

        Ok(shared::count_persisted_tasks(&self.task_store, &job.id).await)
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
            shared::select_provider_id(&job.config, &self.providers, &self.provider_configs, &self.logger).await.ok_or_else(|| {
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
                system_prompt += "\n\nThis is a QUAD setup. Your response MUST include: 'context' (background info), 'question' (specific problem), 'thought' (step-by-step reasoning), and 'answer' (final result). Each field must be a non-empty string. Empty JSON (like {}) is invalid.";
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
            system_prompt += &format!(
                "\n\nYour response MUST be a valid JSON object with the following fields: {}. All fields are required and must be non-empty. Do not return empty JSON like {{}}. Important: Do not include any conversational text outside the JSON block.",
                fields.join(", ")
            );
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

        let timeout_duration = {
            let configs = self.provider_configs.read().await;
            configs
                .iter()
                .find(|c| c.id() == &provider_id)
                .and_then(|c| match c {
                    ProviderConfig::Ollama { .. } => Some(std::time::Duration::from_secs(900)),
                    ProviderConfig::Script { timeout_ms, .. } => timeout_ms
                        .map(|ms| std::time::Duration::from_millis(ms)),
                    _ => None,
                })
                .unwrap_or_else(|| std::time::Duration::from_secs(60))
        };
        let res = match tokio::time::timeout(timeout_duration, provider.generate(req)).await {
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
                    self.logger.log(
                        LogEvent::new(LogLevel::Warn, "task.rate_limited_reject")
                            .with_job(job.id.clone())
                            .with_task(task.id.clone())
                            .with_field("attempts", task.attempts.to_string()),
                    );
                    task.state = TaskState::Rejected;
                    self.metrics.inc_task_rejected();
                    self.task_store.update_task(&task).await?;
                    return Ok(task_id);
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
        if parsed.is_none() {
            let preview: String = raw.chars().take(200).collect();
            self.logger.log(
                LogEvent::new(LogLevel::Warn, "task.parse.failed")
                    .with_job(job.id.clone())
                    .with_task(task.id.clone())
                    .with_dataset_dir(job.config.output.dataset_dir.clone())
                    .with_field("raw_len", raw.len().to_string())
                    .with_field("raw_preview", preview),
            );
        }

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

        let outcome = shared::run_validators(&self.validators, &ctx);
        let issue_codes = outcome
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect::<Vec<_>>()
            .join(",");
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
                .with_field("issues", outcome.issues.len().to_string())
                .with_field("issue_codes", issue_codes),
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
            self.logger.log(
                LogEvent::new(LogLevel::Debug, "task.persist.attempt")
                    .with_job(ctx.job.id.clone())
                    .with_task(task.id.clone())
                    .with_dataset_dir(ctx.job.config.output.dataset_dir.clone()),
            );
            match self.dataset_writer.persist_sample(sample).await {
                Ok(()) => {
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
                }
                Err(e) => {
                    self.logger.log(
                        LogEvent::new(LogLevel::Error, "task.persist.failed")
                            .with_job(ctx.job.id.clone())
                            .with_task(task.id.clone())
                            .with_dataset_dir(ctx.job.config.output.dataset_dir.clone())
                            .with_field("error", format!("{:?}", e)),
                    );
                    return Err(e.into());
                }
            }
        } else {
            let preview: String = res.raw_output.chars().take(300).collect();
            self.logger.log(
                LogEvent::new(LogLevel::Warn, "task.validation.failed")
                    .with_job(ctx.job.id.clone())
                    .with_task(task.id.clone())
                    .with_dataset_dir(ctx.job.config.output.dataset_dir.clone())
                    .with_field("raw_len", res.raw_output.len().to_string())
                    .with_field("raw_preview", preview),
            );
            task.attempts += 1;
            task.raw_response = None;
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

        let provider = create_provider(config.clone(), self.logger.clone());

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

    pub async fn test_provider(&self, config: ProviderConfig) -> anyhow::Result<GenerationResult> {
        use crate::providers::create_provider;
        let provider_id = config.id().clone();
        let model = match &config {
            ProviderConfig::OpenRouter { model, .. } => model.clone(),
            ProviderConfig::Ollama { model, .. } => model.clone(),
            ProviderConfig::Script { .. } => "script".to_string(),
        };
        let provider = create_provider(config, self.logger.clone());
        let request = GenerationRequest {
            provider_id,
            model,
            prompt: PromptSpec {
                system: None,
                user: "hi".to_string(),
                extra: HashMap::new(),
            },
            max_tokens: Some(64),
            temperature: Some(0.2),
            top_p: Some(1.0),
            metadata: HashMap::new(),
        };
        provider
            .generate(request)
            .await
            .map_err(|e| anyhow::anyhow!("{:?}", e))
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

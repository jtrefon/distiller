use crate::domain::{
    Job, JobConfig, JobId, PromptSpec, Sample, Task, TaskState, TemplateConfig, TemplateId,
};
use crate::metrics::Metrics;
use crate::provider::{GenerationRequest, ModelProvider};
use crate::storage::{DatasetWriter, JobStore, TaskStore};
use crate::validation::{ValidationContext, ValidationOutcome, Validator};
use rand::distributions::WeightedIndex;
use rand::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct Orchestrator {
    pub job_store: Arc<dyn JobStore>,
    pub task_store: Arc<dyn TaskStore>,
    pub providers: HashMap<String, Arc<dyn ModelProvider>>,
    pub templates: HashMap<TemplateId, TemplateConfig>,
    pub validators: Vec<Arc<dyn Validator>>,
    pub dataset_writer: Arc<dyn DatasetWriter>,
    pub metrics: Arc<dyn Metrics>,
}

impl Orchestrator {
    pub async fn submit_job(&self, config: JobConfig) -> anyhow::Result<Job> {
        let job = self.job_store.create_job(config).await?;
        self.metrics.inc_job_submitted();
        Ok(job)
    }

    pub fn select_provider_id(&self, config: &JobConfig) -> Option<String> {
        let mut candidates: Vec<(String, f32)> = Vec::new();
        for spec in &config.providers {
            if let Some(p) = self.providers.get(&spec.provider_id) {
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
        let job = self
            .job_store
            .get_job(job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job not found"))?;

        let target = job.config.target_samples;
        let mut created: u64 = 0;

        while created < target {
            let template = self
                .templates
                .get(&job.config.template_id)
                .ok_or_else(|| anyhow::anyhow!("template not found"))?
                .clone();

            let provider_id = self
                .select_provider_id(&job.config)
                .ok_or_else(|| anyhow::anyhow!("no providers available for job"))?;

            let prompt = PromptSpec {
                system: Some(template.system_prompt.clone()),
                user: template.user_prompt_pattern.clone(),
                extra: HashMap::new(),
            };

            let task = Task {
                id: format!("task-{}-{}", job.id, created + 1),
                job_id: job.id.clone(),
                state: TaskState::Queued,
                attempts: 0,
                max_attempts: job.config.validation.max_attempts,
                provider_id: Some(provider_id.clone()),
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

            created += 1;
        }

        loop {
            let next = self.task_store.fetch_next_task().await?;
            if next.is_none() {
                break;
            }
            let mut task = next.unwrap();
            self.metrics.inc_task_started();

            let provider_id = task
                .provider_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("task has no provider"))?;
            let provider = self
                .providers
                .get(&provider_id)
                .ok_or_else(|| anyhow::anyhow!("provider not registered"))?
                .clone();
            let template = self
                .templates
                .get(&task.template_id)
                .ok_or_else(|| anyhow::anyhow!("template not found"))?
                .clone();

            let req = GenerationRequest {
                provider_id: provider_id.clone(),
                model: "mock".to_string(),
                prompt: task.prompt_spec.clone(),
                max_tokens: None,
                temperature: None,
                top_p: None,
                metadata: HashMap::new(),
            };

            let res = provider.generate(req).await?;
            let parsed = serde_json::from_str::<serde_json::Value>(&res.raw_output).ok();

            let ctx = ValidationContext {
                job: job.clone(),
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

            if outcome.passed {
                let sample = Sample {
                    id: format!("sample-{}", task.id),
                    job_id: task.job_id.clone(),
                    task_id: task.id.clone(),
                    provider_id: provider_id.clone(),
                    model_name: "mock".to_string(),
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
            } else {
                task.attempts += 1;
                if task.attempts >= task.max_attempts {
                    task.state = TaskState::Rejected;
                    self.metrics.inc_task_rejected();
                } else {
                    task.state = TaskState::Queued;
                    self.task_store.enqueue_task(task.clone()).await?;
                    self.metrics.inc_task_enqueued();
                }
            }

            self.task_store.update_task(&task).await?;
        }

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
}

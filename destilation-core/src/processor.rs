use crate::domain::{JobId, Task};
use crate::provider::ModelProvider;
use crate::queue::TaskQueue;
use crate::shared;
use crate::storage::{DatasetWriter, JobStore, ProviderStore, TaskStore, TemplateStore};
use crate::validation::Validator;
use crate::logging::SharedEventLogger;
use crate::metrics::Metrics;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;

#[async_trait::async_trait]
pub trait Processor: Send + Sync {
    async fn process_task(&self, task: Task) -> anyhow::Result<()>;
}

pub struct StandardProcessor {
    providers: Arc<RwLock<std::collections::HashMap<String, Arc<dyn ModelProvider>>>>,
    validators: Vec<Arc<dyn Validator>>,
    dataset_writer: Arc<dyn DatasetWriter>,
    task_store: Arc<dyn TaskStore>,
    job_store: Arc<dyn JobStore>,
    metrics: Arc<dyn Metrics>,
    logger: SharedEventLogger,
    processing_timeout: Duration,
}

impl StandardProcessor {
    pub fn new(
        providers: Arc<RwLock<std::collections::HashMap<String, Arc<dyn ModelProvider>>>>,
        validators: Vec<Arc<dyn Validator>>,
        dataset_writer: Arc<dyn DatasetWriter>,
        task_store: Arc<dyn TaskStore>,
        job_store: Arc<dyn JobStore>,
        metrics: Arc<dyn Metrics>,
        logger: SharedEventLogger,
        processing_timeout: Option<Duration>,
    ) -> Self {
        Self {
            providers,
            validators,
            dataset_writer,
            task_store,
            job_store,
            metrics,
            logger,
            processing_timeout: processing_timeout.unwrap_or(Duration::from_secs(300)),
        }
    }
}

pub struct DatasetProcessor {
    job_store: Arc<dyn JobStore>,
    task_store: Arc<dyn TaskStore>,
    provider_store: Arc<dyn ProviderStore>,
    providers: Arc<RwLock<std::collections::HashMap<String, Arc<dyn ModelProvider>>>>,
    provider_configs: Arc<RwLock<Vec<crate::provider::ProviderConfig>>>,
    template_store: Arc<dyn TemplateStore>,
    templates: std::collections::HashMap<crate::domain::TemplateId, crate::domain::TemplateConfig>,
    validators: Vec<Arc<dyn Validator>>,
    dataset_writer: Arc<dyn DatasetWriter>,
    metrics: Arc<dyn Metrics>,
    logger: SharedEventLogger,
}

impl DatasetProcessor {
    pub fn new(
        job_store: Arc<dyn JobStore>,
        task_store: Arc<dyn TaskStore>,
        provider_store: Arc<dyn ProviderStore>,
        providers: Arc<RwLock<std::collections::HashMap<String, Arc<dyn ModelProvider>>>>,
        provider_configs: Arc<RwLock<Vec<crate::provider::ProviderConfig>>>,
        template_store: Arc<dyn TemplateStore>,
        templates: std::collections::HashMap<crate::domain::TemplateId, crate::domain::TemplateConfig>,
        validators: Vec<Arc<dyn Validator>>,
        dataset_writer: Arc<dyn DatasetWriter>,
        metrics: Arc<dyn Metrics>,
        logger: SharedEventLogger,
    ) -> Self {
        Self {
            job_store,
            task_store,
            provider_store,
            providers,
            provider_configs,
            template_store,
            templates,
            validators,
            dataset_writer,
            metrics,
            logger,
        }
    }

    pub async fn process_job(&self, job_id: JobId) -> anyhow::Result<u64> {
        let mut job = self
            .job_store
            .get_job(&job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Job not found"))?;

        job.status = crate::domain::JobStatus::Running;
        self.job_store.update_job(&job).await?;

        // Create a bounded queue for this job
        let queue = Arc::new(crate::queue::bounded_queue(1000, self.logger.clone()));

        // Ensure we have enough tasks enqueued and enqueue them
        shared::ensure_tasks_enqueued(&job, &self.task_store, &self.template_store, &self.templates, &self.providers, &self.provider_configs, &self.metrics, &self.logger).await?;
        let tasks = self.task_store.list_tasks(&job_id).await?;
        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Info,
                "processor.jobs.tasks_enqueued"
            )
            .with_job(job_id.clone())
            .with_field("task_count", tasks.len().to_string()),
        );

        for task in tasks {
            if task.state == crate::domain::TaskState::Queued {
                self.logger.log(
                    crate::logging::LogEvent::new(
                        crate::logging::LogLevel::Debug,
                        "processor.jobs.task_enqueued"
                    )
                    .with_job(job_id.clone())
                    .with_task(task.id.clone()),
                );
                queue.enqueue(task).await?;
            }
        }

        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Info,
                "processor.jobs.queue_length"
            )
            .with_job(job_id.clone())
            .with_field("length", queue.length().await.to_string()),
        );

        // Create a standard processor for this job
        let processor = Arc::new(StandardProcessor::new(
            self.providers.clone(),
            self.validators.clone(),
            self.dataset_writer.clone(),
            self.task_store.clone(),
            self.job_store.clone(),
            self.metrics.clone(),
            self.logger.clone(),
            Some(Duration::from_secs(300)),
        ));

        // Start processor manager with workers
        let mut manager = ProcessorManager::new();
        let worker_count = job.config.max_concurrency.max(1) as usize;
        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Debug,
                "processor.jobs.starting_workers"
            )
            .with_job(job_id.clone())
            .with_field("worker_count", worker_count.to_string()),
        );
        manager.start(worker_count, processor, queue.clone(), self.logger.clone()).await;

        // Wait for job to complete
        loop {
            // Check if job is paused
            job = self.job_store.get_job(&job_id).await?.ok_or_else(|| anyhow::anyhow!("Job not found"))?;
            if job.status == crate::domain::JobStatus::Paused {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }

            // Check if we have enough completed tasks
            let persisted = shared::count_persisted_tasks(&self.task_store, &job_id).await;
            if persisted >= job.config.target_samples {
                self.logger.log(
                    crate::logging::LogEvent::new(
                        crate::logging::LogLevel::Info,
                        "processor.jobs.target_reached"
                    )
                    .with_job(job_id.clone())
                    .with_field("persisted", persisted.to_string())
                    .with_field("target", job.config.target_samples.to_string()),
                );
                break;
            }

            // Check if there are any more tasks to process
            let remaining_tasks = self.task_store.list_tasks(&job_id).await?
                .iter()
                .filter(|t| t.state == crate::domain::TaskState::Queued || t.state == crate::domain::TaskState::Generating || t.state == crate::domain::TaskState::Validating)
                .count();

            self.logger.log(
                crate::logging::LogEvent::new(
                    crate::logging::LogLevel::Debug,
                    "processor.jobs.remaining_tasks"
                )
                .with_job(job_id.clone())
                .with_field("count", remaining_tasks.to_string())
                .with_field("queue_length", queue.length().await.to_string()),
            );

            if remaining_tasks == 0 {
                self.logger.log(
                    crate::logging::LogEvent::new(
                        crate::logging::LogLevel::Info,
                        "processor.jobs.no_more_tasks"
                    )
                    .with_job(job_id.clone()),
                );
                break;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        // Stop the processor manager
        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Info,
                "processor.jobs.stopping_workers"
            )
            .with_job(job_id.clone()),
        );
        manager.stop().await;

        // Update job status
        job = self.job_store.get_job(&job_id).await?.ok_or_else(|| anyhow::anyhow!("Job not found"))?;
        let persisted = shared::count_persisted_tasks(&self.task_store, &job_id).await;
        if persisted >= job.config.target_samples {
            job.status = crate::domain::JobStatus::Completed;
        } else {
            job.status = crate::domain::JobStatus::Failed;
        }
        job.completed_samples = persisted;
        job.updated_at = chrono::Utc::now();
        job.finished_at = Some(chrono::Utc::now());
        self.job_store.update_job(&job).await?;

        Ok(persisted)
    }



}

#[async_trait::async_trait]
impl Processor for StandardProcessor {
    async fn process_task(&self, mut task: Task) -> anyhow::Result<()> {
        self.metrics.inc_task_started();
        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Info,
                "processor.task.started",
            )
            .with_job(task.job_id.clone())
            .with_task(task.id.clone()),
        );

        let result = tokio::time::timeout(
            self.processing_timeout,
            self.process_task_inner(&mut task),
        )
        .await;

        match result {
            Ok(Ok(())) => {
                self.logger.log(
                    crate::logging::LogEvent::new(
                        crate::logging::LogLevel::Info,
                        "processor.task.completed",
                    )
                    .with_job(task.job_id.clone())
                    .with_task(task.id.clone()),
                );
                Ok(())
            }
            Ok(Err(e)) => {
                self.logger.log(
                    crate::logging::LogEvent::new(
                        crate::logging::LogLevel::Error,
                        "processor.task.failed",
                    )
                    .with_job(task.job_id.clone())
                    .with_task(task.id.clone())
                    .with_field("error", e.to_string()),
                );
                Err(e)
            }
            Err(_) => {
                let error = anyhow::anyhow!("Processing timed out");
                self.logger.log(
                    crate::logging::LogEvent::new(
                        crate::logging::LogLevel::Error,
                        "processor.task.timeout",
                    )
                    .with_job(task.job_id.clone())
                    .with_task(task.id.clone())
                    .with_field("error", error.to_string()),
                );
                Err(error)
            }
        }
    }
}

impl StandardProcessor {
    async fn process_task_inner(&self, task: &mut Task) -> anyhow::Result<()> {
        // Select provider
        let provider = if let Some(provider_id) = &task.provider_id {
            let providers = self.providers.read().await;
            providers.get(provider_id).ok_or_else(|| {
                anyhow::anyhow!("Provider '{}' not found", provider_id)
            })?
            .clone()
        } else {
            return Err(anyhow::anyhow!("No provider specified"));
        };

        // Generate
        let generation_result = match provider.generate(crate::provider::GenerationRequest {
            provider_id: provider.metadata().id.clone(),
            model: provider.metadata().models.first().cloned().ok_or_else(|| {
                anyhow::anyhow!("No model available for provider")
            })?,
            prompt: task.prompt_spec.clone(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            metadata: Default::default(),
        }).await {
            Ok(result) => {
                self.logger.log(
                    crate::logging::LogEvent::new(
                        crate::logging::LogLevel::Debug,
                        "processor.task.response_received",
                    )
                    .with_job(task.job_id.clone())
                    .with_task(task.id.clone())
                    .with_field("raw_output", result.raw_output.clone())
                    .with_field("output_length", result.raw_output.len().to_string()),
                );
                result
            }
            Err(e) => {
                // Handle provider errors
                if let crate::provider::ProviderError::Critical(msg) = &e {
                    // Pause job immediately
                    if let Some(mut job) = self.job_store.get_job(&task.job_id).await? {
                        job.status = crate::domain::JobStatus::Paused;
                        if let Err(err) = self.job_store.update_job(&job).await {
                            self.logger.log(
                                crate::logging::LogEvent::new(
                                    crate::logging::LogLevel::Error,
                                    "job.pause_failed",
                                )
                                .with_job(task.job_id.clone())
                                .with_field("error", err.to_string()),
                            );
                        }
                    }
                    
                    self.logger.log(
                        crate::logging::LogEvent::new(
                            crate::logging::LogLevel::Error,
                            "job.error",
                        )
                        .with_job(task.job_id.clone())
                        .with_field("error", msg.clone()),
                    );
                    
                    // Don't requeue task immediately, let resume handle it
                    // But we need to update state so it's not lost
                    task.state = crate::domain::TaskState::Queued;
                    self.task_store.update_task(task).await?;
                    return Err(anyhow::anyhow!("{}", msg));
                }
                
                return Err(anyhow::anyhow!("Provider error: {:?}", e));
            }
        };

        task.raw_response = Some(generation_result.raw_output.clone());
        task.attempts += 1;

        // Validate
        let validation_context = crate::validation::ValidationContext {
            job: self.job_store.get_job(&task.job_id).await?.ok_or_else(|| {
                anyhow::anyhow!("Job '{}' not found", task.job_id)
            })?,
            task: task.clone(),
            template: crate::domain::TemplateConfig {
                id: task.template_id.clone(),
                name: "Default Template".to_string(),
                description: "Default template".to_string(),
                mode: crate::domain::TemplateMode::Simple,
                schema: crate::domain::TemplateSchema {
                    version: "v1".to_string(),
                    fields: vec![],
                    json_schema: None,
                },
                system_prompt: task.prompt_spec.system.clone().unwrap_or_default(),
                user_prompt_pattern: task.prompt_spec.user.clone(),
                examples: vec![],
                validators: vec![],
            },
            provider_result: generation_result,
            parsed_output: serde_json::from_str(&task.raw_response.as_ref().unwrap()).ok(),
        };

        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Debug,
                "processor.task.validation_started",
            )
            .with_job(task.job_id.clone())
            .with_task(task.id.clone())
            .with_field("parsed_output", format!("{:?}", validation_context.parsed_output)),
        );

        let validation_outcome = shared::run_validators(&self.validators, &validation_context);
        if validation_outcome.passed {
            self.metrics.record_validator_pass();
        } else {
            self.metrics.record_validator_fail();
        }

        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Debug,
                "processor.task.validation_completed",
            )
            .with_job(task.job_id.clone())
            .with_task(task.id.clone())
            .with_field("passed", validation_outcome.passed.to_string())
            .with_field("issues", format!("{:?}", validation_outcome.issues))
            .with_field("score", format!("{:?}", validation_outcome.score)),
        );

        if validation_outcome.passed {
            // Save sample
            let sample = crate::domain::Sample {
                id: format!("sample-{}", task.id),
                job_id: task.job_id.clone(),
                task_id: task.id.clone(),
                provider_id: provider.metadata().id.clone(),
                model_name: provider.metadata().models.first().cloned().unwrap_or_default(),
                template_id: task.template_id.clone(),
                schema_version: "v1".to_string(),
                payload: validation_context.parsed_output.unwrap_or(serde_json::Value::Null),
                quality_score: validation_outcome.score,
                is_negative: false,
                tags: vec![],
            };

            self.logger.log(
                crate::logging::LogEvent::new(
                    crate::logging::LogLevel::Debug,
                    "processor.task.persist.attempt",
                )
                .with_job(task.job_id.clone())
                .with_task(task.id.clone()),
            );
            match self.dataset_writer.persist_sample(sample).await {
                Ok(()) => {
                    task.state = crate::domain::TaskState::Persisted;
                    self.metrics.inc_samples_persisted();
                    self.metrics.inc_task_persisted();
                }
                Err(e) => {
                    self.logger.log(
                        crate::logging::LogEvent::new(
                            crate::logging::LogLevel::Error,
                            "processor.task.persist.failed",
                        )
                        .with_job(task.job_id.clone())
                        .with_task(task.id.clone())
                        .with_field("error", format!("{:?}", e)),
                    );
                    return Err(e.into());
                }
            }
        } else {
            if task.attempts >= task.max_attempts {
                task.state = crate::domain::TaskState::Rejected;
                self.metrics.inc_task_rejected();
            } else {
                task.state = crate::domain::TaskState::Queued;
            }
            task.validation_result = Some(validation_outcome);
        }

        self.task_store.update_task(task).await?;
        Ok(())
    }

}

pub struct ProcessorWorker {
    processor: Arc<dyn Processor>,
    queue: Arc<dyn TaskQueue>,
    logger: SharedEventLogger,
    worker_id: u64,
    shutdown: tokio::sync::oneshot::Receiver<()>,
}

impl ProcessorWorker {
    pub fn new(
        processor: Arc<dyn Processor>,
        queue: Arc<dyn TaskQueue>,
        logger: SharedEventLogger,
        worker_id: u64,
        shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Self {
        Self {
            processor,
            queue,
            logger,
            worker_id,
            shutdown,
        }
    }

    pub async fn run(mut self) {
        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Debug,
                "processor.worker.started",
            )
            .with_field("worker_id", self.worker_id.to_string()),
        );

        loop {
            tokio::select! {
                _ = &mut self.shutdown => {
                    self.logger.log(
                        crate::logging::LogEvent::new(
                            crate::logging::LogLevel::Debug,
                            "processor.worker.shutdown",
                        )
                        .with_field("worker_id", self.worker_id.to_string()),
                    );
                    break;
                }
                task_result = self.queue.dequeue() => {
                    match task_result {
                        Ok(task) => {
                            self.logger.log(
                                crate::logging::LogEvent::new(
                                    crate::logging::LogLevel::Debug,
                                    "processor.worker.task_received",
                                )
                                .with_field("worker_id", self.worker_id.to_string())
                                .with_task(task.id.clone()),
                            );
                            if let Err(e) = self.processor.process_task(task).await {
                                self.logger.log(
                                    crate::logging::LogEvent::new(
                                        crate::logging::LogLevel::Error,
                                        "processor.worker.error",
                                    )
                                    .with_field("worker_id", self.worker_id.to_string())
                                    .with_field("error", e.to_string()),
                                );
                            }
                        }
                        Err(e) => {
                            self.logger.log(
                                crate::logging::LogEvent::new(
                                    crate::logging::LogLevel::Warn,
                                    "processor.worker.dequeue_error",
                                )
                                .with_field("worker_id", self.worker_id.to_string())
                                .with_field("error", e.to_string()),
                            );
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        }

        self.logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Debug,
                "processor.worker.stopped",
            )
            .with_field("worker_id", self.worker_id.to_string()),
        );
    }
}

pub struct ProcessorManager {
    workers: Vec<tokio::task::JoinHandle<()>>,
    shutdown_senders: Vec<tokio::sync::oneshot::Sender<()>>,
}

impl ProcessorManager {
    pub fn new() -> Self {
        Self {
            workers: Vec::new(),
            shutdown_senders: Vec::new(),
        }
    }

    pub async fn start(
        &mut self,
        worker_count: usize,
        processor: Arc<dyn Processor>,
        queue: Arc<dyn TaskQueue>,
        logger: SharedEventLogger,
    ) {
        for i in 0..worker_count {
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            let worker = ProcessorWorker::new(
                processor.clone(),
                queue.clone(),
                logger.clone(),
                i as u64,
                shutdown_rx,
            );
            let handle = tokio::spawn(worker.run());
            self.workers.push(handle);
            self.shutdown_senders.push(shutdown_tx);
            logger.log(
                crate::logging::LogEvent::new(
                    crate::logging::LogLevel::Debug,
                    "processor.manager.worker_spawned",
                )
                .with_field("worker_id", i.to_string()),
            );
        }

        logger.log(
            crate::logging::LogEvent::new(
                crate::logging::LogLevel::Info,
                "processor.manager.started",
            )
            .with_field("worker_count", worker_count.to_string()),
        );
    }

    pub async fn stop(&mut self) {
        for tx in self.shutdown_senders.drain(..) {
            let _ = tx.send(());
        }

        for handle in self.workers.drain(..) {
            let _ = handle.await;
        }
    }
}

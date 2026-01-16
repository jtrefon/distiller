use destilation_core::domain::{
    DomainSpec, JobConfig, JobOutputConfig, JobProviderSpec, JobValidationConfig, PromptSpec,
    ReasoningMode, Task, TaskState, TemplateId,
};
use destilation_core::storage::{JobStore, SqliteJobStore, SqliteTaskStore, TaskStore};
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashMap;

async fn create_pool() -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .expect("Failed to create pool")
}

fn create_sample_job_config(id: &str) -> JobConfig {
    JobConfig {
        id: id.to_string(),
        name: "Test Job".to_string(),
        description: None,
        target_samples: 10,
        max_concurrency: 1,
        domains: vec![DomainSpec {
            id: "d1".to_string(),
            name: "Domain 1".to_string(),
            weight: 1.0,
            tags: vec![],
        }],
        template_id: TemplateId::from("t1"),
        reasoning_mode: ReasoningMode::Simple,
        providers: vec![JobProviderSpec {
            provider_id: "mock".to_string(),
            weight: 1.0,
            capabilities_required: vec![],
        }],
        validation: JobValidationConfig {
            max_attempts: 1,
            validators: vec![],
            min_quality_score: None,
            fail_fast: false,
        },
        output: JobOutputConfig {
            dataset_dir: "tmp".to_string(),
            shard_size: 100,
            compress: false,
            metadata: HashMap::new(),
        },
    }
}

#[tokio::test]
async fn test_sqlite_job_store() -> anyhow::Result<()> {
    let pool = create_pool().await;
    let store = SqliteJobStore::new(pool.clone());
    store.init().await?;

    // List empty
    let empty_jobs = store.list_jobs().await?;
    assert!(empty_jobs.is_empty());

    // Get missing
    let missing = store.get_job(&"missing".to_string()).await?;
    assert!(missing.is_none());

    // Create
    let config = create_sample_job_config("job-1");
    let job = store.create_job(config.clone()).await?;
    assert_eq!(job.id, "job-1");
    assert_eq!(job.completed_samples, 0);

    // Get
    let fetched = store.get_job(&"job-1".to_string()).await?;
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().id, "job-1");

    // List
    let jobs = store.list_jobs().await?;
    assert_eq!(jobs.len(), 1);

    // Update
    let mut job = job;
    job.completed_samples = 5;
    store.update_job(&job).await?;

    let updated = store.get_job(&"job-1".to_string()).await?.unwrap();
    assert_eq!(updated.completed_samples, 5);

    Ok(())
}

#[tokio::test]
async fn test_sqlite_task_store() -> anyhow::Result<()> {
    let pool = create_pool().await;
    let store = SqliteTaskStore::new(pool.clone());
    store.init().await?;

    // Verify empty fetch
    let empty = store.fetch_next_task().await?;
    assert!(empty.is_none());

    let task = Task {
        id: "task-1".to_string(),
        job_id: "job-1".to_string(),
        state: TaskState::Queued,
        attempts: 0,
        max_attempts: 3,
        provider_id: None,
        domain_id: "d1".to_string(),
        template_id: "t1".to_string(),
        prompt_spec: PromptSpec {
            system: None,
            user: "test prompt".to_string(),
            extra: HashMap::new(),
        },
        raw_response: None,
        validation_result: None,
    };

    // Enqueue
    store.enqueue_task(task.clone()).await?;

    // Fetch
    let fetched = store.fetch_next_task().await?;
    assert!(fetched.is_some());
    let mut fetched_task = fetched.unwrap();
    assert_eq!(fetched_task.id, "task-1");
    assert_eq!(fetched_task.state, TaskState::Generating); // Should be marked Generating

    // Update
    fetched_task.state = TaskState::Persisted;
    fetched_task.raw_response = Some("response".to_string());
    store.update_task(&fetched_task).await?;

    // Verify update (should not be fetched again as Queued)
    let next = store.fetch_next_task().await?;
    assert!(next.is_none());

    Ok(())
}

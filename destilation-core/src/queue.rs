use crate::domain::Task;
use crate::logging::{LogEvent, LogLevel, SharedEventLogger, NoopEventLogger};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    Mutex,
};
use tokio::time::Duration;

/// Represents a task queue for managing task processing
#[async_trait]
pub trait TaskQueue: Send + Sync {
    /// Enqueue a task for processing
    async fn enqueue(&self, task: Task) -> anyhow::Result<()>;

    /// Dequeue a task for processing
    async fn dequeue(&self) -> anyhow::Result<Task>;

    /// Get the current queue length
    async fn length(&self) -> usize;

    /// Check if the queue is empty
    async fn is_empty(&self) -> bool;
}

/// Configuration for the in-memory task queue
#[derive(Debug, Clone)]
pub struct MemoryQueueConfig {
    /// Maximum queue capacity (0 for unbounded)
    pub capacity: usize,

    /// Timeout for dequeue operations
    pub dequeue_timeout: Duration,
}

impl Default for MemoryQueueConfig {
    fn default() -> Self {
        Self {
            capacity: 0, // Unbounded by default
            dequeue_timeout: Duration::from_secs(30),
        }
    }
}

/// In-memory implementation of the TaskQueue trait
pub struct MemoryTaskQueue {
    config: MemoryQueueConfig,
    sender: UnboundedSender<Task>,
    receiver: Arc<Mutex<UnboundedReceiver<Task>>>,
    length: tokio::sync::watch::Receiver<usize>,
    length_sender: tokio::sync::watch::Sender<usize>,
    logger: SharedEventLogger,
}

impl MemoryTaskQueue {
    /// Create a new in-memory task queue
    pub fn new(config: Option<MemoryQueueConfig>, logger: SharedEventLogger) -> Self {
        let config = config.unwrap_or_default();
        let (sender, receiver) = unbounded_channel();
        let (length_sender, length_receiver) = tokio::sync::watch::channel(0);

        Self {
            config,
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
            length: length_receiver,
            length_sender,
            logger,
        }
    }

    /// Create a new in-memory task queue with default configuration
    pub fn default(logger: SharedEventLogger) -> Self {
        Self::new(None, logger)
    }
}

#[async_trait]
impl TaskQueue for MemoryTaskQueue {
    async fn enqueue(&self, task: Task) -> anyhow::Result<()> {
        // Check capacity if bounded
        if self.config.capacity > 0 {
            let current_length = *self.length.borrow();
            if current_length >= self.config.capacity {
                self.logger.log(LogEvent::new(LogLevel::Warn, "queue.capacity_exceeded").with_field("task_id", task.id.clone()));
                return Err(anyhow::anyhow!("Queue capacity exceeded"));
            }
        }

        // Enqueue the task
        self.sender
            .send(task)
            .map_err(|e| anyhow::anyhow!("Failed to enqueue task: {}", e))?;

        // Update length
        let current_length = *self.length.borrow();
        let _ = self.length_sender.send(current_length + 1);

        Ok(())
    }

    async fn dequeue(&self) -> anyhow::Result<Task> {
        let mut receiver = self.receiver.lock().await;

        self.logger.log(LogEvent::new(LogLevel::Debug, "queue.dequeue.attempt"));

        let task = tokio::time::timeout(
            self.config.dequeue_timeout,
            receiver.recv(),
        )
        .await
        .map_err(|_| {
            self.logger.log(LogEvent::new(LogLevel::Warn, "queue.dequeue.timeout"));
            anyhow::anyhow!("Dequeue timeout")
        })?
        .ok_or_else(|| {
            self.logger.log(LogEvent::new(LogLevel::Error, "queue.dequeue.closed"));
            anyhow::anyhow!("Queue receiver closed")
        })?;

        self.logger.log(LogEvent::new(LogLevel::Info, "queue.dequeue.success").with_field("task_id", task.id.clone()));

        // Update length
        let current_length = *self.length.borrow();
        let _ = self.length_sender.send(current_length.saturating_sub(1));

        self.logger.log(LogEvent::new(LogLevel::Debug, "queue.length.updated").with_field("length", current_length.saturating_sub(1).to_string()));

        Ok(task)
    }

    async fn length(&self) -> usize {
        *self.length.borrow()
    }

    async fn is_empty(&self) -> bool {
        *self.length.borrow() == 0
    }
}

/// Bounded version of the in-memory queue
pub fn bounded_queue(capacity: usize, logger: SharedEventLogger) -> MemoryTaskQueue {
    MemoryTaskQueue::new(Some(MemoryQueueConfig {
        capacity,
        ..Default::default()
    }), logger)
}

/// Unbounded version of the in-memory queue (default)
pub fn unbounded_queue(logger: SharedEventLogger) -> MemoryTaskQueue {
    MemoryTaskQueue::new(None, logger)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        JobId, Task, TaskId, TaskState, TemplateId,
    };

    fn create_test_task(id: TaskId) -> Task {
        Task {
            id,
            job_id: JobId::from("test-job"),
            state: TaskState::Queued,
            attempts: 0,
            max_attempts: 3,
            provider_id: None,
            domain_id: "test-domain".to_string(),
            template_id: TemplateId::from("test-template"),
            prompt_spec: crate::domain::PromptSpec {
                system: None,
                user: "test prompt".to_string(),
                extra: Default::default(),
            },
            raw_response: None,
            validation_result: None,
            quality_score: None,
            is_negative: false,
        }
    }

    #[tokio::test]
    async fn test_enqueue_dequeue() {
        let logger = Arc::new(NoopEventLogger);
        let queue = MemoryTaskQueue::default(logger);

        let task1 = create_test_task("task-1".to_string());
        let task2 = create_test_task("task-2".to_string());

        queue.enqueue(task1.clone()).await.unwrap();
        queue.enqueue(task2.clone()).await.unwrap();

        assert_eq!(queue.length().await, 2);
        assert!(!queue.is_empty().await);

        let dequeued1 = queue.dequeue().await.unwrap();
        let dequeued2 = queue.dequeue().await.unwrap();

        assert_eq!(dequeued1.id, task1.id);
        assert_eq!(dequeued2.id, task2.id);

        assert_eq!(queue.length().await, 0);
        assert!(queue.is_empty().await);
    }

    #[tokio::test]
    async fn test_bounded_queue() {
        let logger = Arc::new(NoopEventLogger);
        let queue = bounded_queue(2, logger);

        let task1 = create_test_task("task-1".to_string());
        let task2 = create_test_task("task-2".to_string());
        let task3 = create_test_task("task-3".to_string());

        queue.enqueue(task1).await.unwrap();
        queue.enqueue(task2).await.unwrap();

        assert_eq!(queue.length().await, 2);

        // Should fail to enqueue third task
        let result = queue.enqueue(task3).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Queue capacity exceeded");
    }

    #[tokio::test]
    async fn test_dequeue_timeout() {
        let logger = Arc::new(NoopEventLogger);
        let config = MemoryQueueConfig {
            capacity: 0,
            dequeue_timeout: Duration::from_millis(100),
        };
        let queue = MemoryTaskQueue::new(Some(config), logger);

        let result = queue.dequeue().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Dequeue timeout"));
    }
}

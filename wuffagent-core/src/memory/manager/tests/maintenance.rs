use super::*;

// --- Maintenance pass tests ---

/// Mock LLM returning a queued sequence of maintenance-actions JSON
/// responses (one per LLM call; falls back to `{}` when exhausted).
/// Records the call count and every prompt for batch assertions.
struct QueuedLlm {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    calls: std::sync::atomic::AtomicUsize,
    prompts: std::sync::Mutex<Vec<String>>,
}

impl QueuedLlm {
    fn new(responses: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            responses: std::sync::Mutex::new(std::collections::VecDeque::from(responses)),
            calls: std::sync::atomic::AtomicUsize::new(0),
            prompts: std::sync::Mutex::new(Vec::new()),
        })
    }
    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn first_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
    }
    fn last_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl LlmClient for QueuedLlm {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.prompts.lock().unwrap().push(
            messages
                .first()
                .map(|m| m.content.clone())
                .unwrap_or_default(),
        );
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| "{}".to_string()))
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let text = self.complete(messages).await?;
        chunk_handler(text.clone());
        Ok(text)
    }
}

/// Build a manager with a queued-response LLM and a specific batch size.
fn manager_with_queued_llm(
    dir: &std::path::Path,
    responses: Vec<String>,
    batch_size: usize,
) -> (MemoryManager, Arc<QueuedLlm>) {
    let llm = QueuedLlm::new(responses);
    let config = MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        memory_maintenance: true,
        memory_maintenance_batch_size: batch_size,
        ..Default::default()
    };
    (
        MemoryManager::new_with_llm(config, llm.clone()).unwrap(),
        llm,
    )
}

/// Seed `n` mutually distinct entries with known IDs ("m-0", "m-1", ...).
/// Content has low token overlap so the add-time dedup gate never fires.
fn seed_distinct_with_ids(manager: &MemoryManager, n: usize) {
    for i in 0..n {
        manager
            .add(entry_with_id(
                &format!("m-{i}"),
                MemoryType::Fact,
                &format!(
                    "Distinct memory {i} covering topic alpha{i} with details beta{i} gamma{i} delta{i} epsilon{i}"
                ),
                &[],
            ))
            .unwrap();
    }
}

/// Mock LLM returning a fixed maintenance-actions JSON response.
struct ScriptedLlm {
    response: String,
}

#[async_trait::async_trait]
impl LlmClient for ScriptedLlm {
    async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
        Ok(self.response.clone())
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let text = self.complete(messages).await?;
        chunk_handler(text.clone());
        Ok(text)
    }
}

fn manager_with_llm(dir: &std::path::Path, response: &str) -> MemoryManager {
    let config = MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        memory_maintenance: true,
        ..Default::default()
    };
    MemoryManager::new_with_llm(
        config,
        Arc::new(ScriptedLlm {
            response: response.to_string(),
        }),
    )
    .unwrap()
}

/// Build an entry with a known ID so scripted LLM responses can target it.
fn entry_with_id(id: &str, r#type: MemoryType, content: &str, tags: &[&str]) -> MemoryEntry {
    let mut e = MemoryEntry::new(r#type, content, "test", tags);
    e.id = id.to_string();
    e
}

/// Seed the standard three-entry fixture into `manager`.
fn seed_three(manager: &MemoryManager) {
    for e in [
        entry_with_id(
            "mem-a",
            MemoryType::Fact,
            "The build uses cargo with a workspace layout for core and gui",
            &["build"],
        ),
        entry_with_id(
            "mem-b",
            MemoryType::Fact,
            "The release notes must mention the new memory panel feature",
            &["release"],
        ),
        entry_with_id(
            "mem-c",
            MemoryType::Lesson,
            "Use PowerShell not bash when running commands on this machine",
            &["shell"],
        ),
    ] {
        manager.add(e).unwrap();
    }
}

#[tokio::test]
async fn test_maintenance_merge() {
    let dir = tempdir().unwrap();
    let response = r#"{"merge": [{"ids": ["mem-a", "mem-b"], "consolidated": "Consolidated build and release note"}]}"#;
    let manager = manager_with_llm(dir.path(), response);
    seed_three(&manager);

    let report = manager.run_maintenance().await.unwrap();
    assert!(
        report.summary.contains("merged"),
        "summary: {}",
        report.summary
    );
    assert!(manager.find("mem-a").is_none(), "source a must be removed");
    assert!(manager.find("mem-b").is_none(), "source b must be removed");
    assert!(
        manager.find("mem-c").is_some(),
        "unrelated entry must survive"
    );
    // The consolidated entry replaced the two sources: net -1.
    assert_eq!(manager.count(), 2);
}

#[tokio::test]
async fn test_maintenance_update_and_delete() {
    let dir = tempdir().unwrap();
    let response = r#"{"update": [{"id": "mem-a", "content": "Updated build note content", "tags": ["build", "v2"]}], "delete": [{"id": "mem-b", "reason": "stale"}]}"#;
    let manager = manager_with_llm(dir.path(), response);
    seed_three(&manager);

    let report = manager.run_maintenance().await.unwrap();
    assert!(
        report.summary.contains("updated"),
        "summary: {}",
        report.summary
    );
    assert!(
        report.summary.contains("deleted"),
        "summary: {}",
        report.summary
    );

    let updated = manager.find("mem-a").unwrap();
    assert_eq!(updated.content, "Updated build note content");
    assert!(updated.tags.contains(&"v2".to_string()));
    assert!(
        manager.find("mem-b").is_none(),
        "deleted entry must be gone"
    );
    assert!(manager.find("mem-c").is_some());
}

#[tokio::test]
async fn test_maintenance_unknown_and_malformed_actions() {
    let dir = tempdir().unwrap();
    // Malformed JSON parses to no actions: a no-op, not an error.
    let manager = manager_with_llm(dir.path(), "not json at all");
    seed_three(&manager);
    let report = manager.run_maintenance().await.unwrap();
    assert_eq!(manager.count(), 3);
    assert!(
        report.summary.contains("no changes suggested"),
        "summary: {}",
        report.summary
    );

    // Unknown IDs in every action type: no-op.
    let response = r#"{"merge": [{"ids": ["nope1", "nope2"], "consolidated": "x"}], "update": [{"id": "nope3", "content": "y"}], "delete": [{"id": "nope4", "reason": "r"}]}"#;
    let manager = manager_with_llm(dir.path(), response);
    seed_three(&manager);
    let report = manager.run_maintenance().await.unwrap();
    assert_eq!(manager.count(), 3, "unknown ids must not mutate the store");
    assert!(
        report.summary.contains("no changes applied"),
        "summary: {}",
        report.summary
    );
}

#[tokio::test]
async fn test_maintenance_never_wipes_store() {
    let dir = tempdir().unwrap();
    // Delete all three: the last remaining one must be kept.
    let response = r#"{"delete": [{"id": "mem-a"}, {"id": "mem-b"}, {"id": "mem-c"}]}"#;
    let manager = manager_with_llm(dir.path(), response);
    seed_three(&manager);

    let report = manager.run_maintenance().await.unwrap();
    assert_eq!(
        manager.count(),
        1,
        "store must never be emptied: {}",
        report.summary
    );
}

#[tokio::test]
async fn test_maintenance_requires_llm_client() {
    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    assert!(manager.run_maintenance().await.is_err());
    assert!(manager.run_maintenance_step().await.is_err());
}

// --- Batched maintenance tests ---

#[tokio::test]
async fn test_maintenance_batched_chunks_oldest_first() {
    let dir = tempdir().unwrap();
    // 35 entries, batch 15 => chunks of 15 + 15 + 5 => exactly 3 LLM calls.
    let (manager, llm) = manager_with_queued_llm(dir.path(), vec!["{}".to_string()], 15);
    seed_distinct_with_ids(&manager, 35);

    let report = manager.run_maintenance().await.unwrap();
    assert_eq!(llm.call_count(), 3, "35/15 must be 3 batches (15+15+5)");
    assert_eq!(report.batches, 3);
    // Oldest-first: the first prompt holds the oldest entries only.
    assert!(llm.first_prompt().contains("Distinct memory 0"));
    assert!(!llm.first_prompt().contains("Distinct memory 15"));
    // The last batch is the newest entries.
    assert!(llm.last_prompt().contains("Distinct memory 34"));
    // "{}" from every batch: no changes, store intact.
    assert_eq!(manager.count(), 35);
    assert!(
        report.summary.contains("no changes suggested"),
        "summary: {}",
        report.summary
    );
}

#[tokio::test]
async fn test_maintenance_batched_applies_each_batch() {
    let dir = tempdir().unwrap();
    // 20 entries, batch 10 => 2 calls; each batch gets its own response.
    let responses = vec![
        r#"{"merge": [{"ids": ["m-0", "m-1"], "consolidated": "Consolidated oldest pair"}]}"#
            .to_string(),
        r#"{"update": [{"id": "m-15", "content": "Updated middle entry content", "tags": ["t"]}]}"#
            .to_string(),
    ];
    let (manager, llm) = manager_with_queued_llm(dir.path(), responses, 10);
    seed_distinct_with_ids(&manager, 20);

    let report = manager.run_maintenance().await.unwrap();
    assert_eq!(llm.call_count(), 2);
    // Batch 1 actions applied to the oldest chunk...
    assert!(
        manager.find("m-0").is_none(),
        "merge source m-0 must be removed"
    );
    assert!(
        manager.find("m-1").is_none(),
        "merge source m-1 must be removed"
    );
    // ...and batch 2 actions to the newest chunk.
    let updated = manager.find("m-15").expect("m-15 must survive");
    assert_eq!(updated.content, "Updated middle entry content");
    // 20 - 2 merged sources + 1 consolidated = 19.
    assert_eq!(manager.count(), 19);
    assert!(
        report.summary.contains("merged"),
        "summary: {}",
        report.summary
    );
    assert!(
        report.summary.contains("updated"),
        "summary: {}",
        report.summary
    );
    assert_eq!(
        report.merges, 1,
        "one merge action (covering two source ids)"
    );
    assert_eq!(report.updated, 1);
}

#[tokio::test]
async fn test_maintenance_batched_never_wipes_store() {
    let dir = tempdir().unwrap();
    // Every batch tries to delete all of ITS entries: the very last
    // remaining entry in the store must be kept.
    let del_0_9 = (0..10)
        .map(|i| format!("{{\"id\": \"m-{i}\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    let del_10_19 = (10..20)
        .map(|i| format!("{{\"id\": \"m-{i}\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    let responses = vec![
        format!("{{\"delete\": [{del_0_9}]}}"),
        format!("{{\"delete\": [{del_10_19}]}}"),
    ];
    let (manager, llm) = manager_with_queued_llm(dir.path(), responses, 10);
    seed_distinct_with_ids(&manager, 20);

    let report = manager.run_maintenance().await.unwrap();
    assert_eq!(llm.call_count(), 2);
    assert_eq!(
        manager.count(),
        1,
        "store must never be emptied: {}",
        report.summary
    );
}

#[tokio::test]
async fn test_maintenance_batched_skips_single_entry_chunk() {
    let dir = tempdir().unwrap();
    // 16 entries, batch 15 => 15 + 1; the lone leftover cannot be merged
    // with anything, so only ONE LLM call happens.
    let (manager, llm) = manager_with_queued_llm(dir.path(), vec!["{}".to_string()], 15);
    seed_distinct_with_ids(&manager, 16);

    let report = manager.run_maintenance().await.unwrap();
    assert_eq!(llm.call_count(), 1, "single-entry chunk must be skipped");
    assert_eq!(report.batches, 1);
    assert_eq!(manager.count(), 16);
}

#[tokio::test]
async fn test_maintenance_step_processes_only_oldest_batch() {
    let dir = tempdir().unwrap();
    // 20 entries, batch 10: the post-task step must make exactly ONE call
    // covering only the oldest 10 entries.
    let (manager, llm) = manager_with_queued_llm(dir.path(), vec!["{}".to_string()], 10);
    seed_distinct_with_ids(&manager, 20);

    let report = manager.run_maintenance_step().await.unwrap();
    assert_eq!(llm.call_count(), 1, "step must be a single LLM call");
    assert!(llm.first_prompt().contains("Distinct memory 0"));
    assert!(!llm.first_prompt().contains("Distinct memory 19"));
    assert_eq!(report.batches, 1);
    assert_eq!(manager.count(), 20);
}

#[tokio::test]
async fn test_maintenance_step_applies_actions() {
    let dir = tempdir().unwrap();
    let responses = vec![
        r#"{"merge": [{"ids": ["m-0", "m-1"], "consolidated": "Consolidated oldest pair"}]}"#
            .to_string(),
    ];
    let (manager, llm) = manager_with_queued_llm(dir.path(), responses, 10);
    seed_distinct_with_ids(&manager, 20);

    let report = manager.run_maintenance_step().await.unwrap();
    assert_eq!(llm.call_count(), 1);
    assert!(manager.find("m-0").is_none());
    assert!(manager.find("m-1").is_none());
    // Newest entries must be untouched by a step.
    assert!(manager.find("m-19").is_some());
    assert_eq!(manager.count(), 19);
    assert!(
        report.summary.contains("merged"),
        "summary: {}",
        report.summary
    );
}

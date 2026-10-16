//! One background queue per agent; polling never waits for model inference.
use super::{ctx::Ctx, sessions::Resumable};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[derive(Default)]
pub struct SummaryCache(Mutex<State>);
#[derive(Default)]
struct State {
    entries: HashMap<PathBuf, Entry>,
    queue: VecDeque<Job>,
    running: bool,
}
struct Entry {
    observed: (SystemTime, u64),
    summary: Option<String>,
    busy: bool,
    failed: bool,
    retry_at: Instant,
}
struct Job {
    source: PathBuf,
    cache: PathBuf,
}

fn cache_file(ctx: &Ctx, source: &std::path::Path) -> PathBuf {
    let key = format!(
        "{:x}",
        Sha256::digest(source.as_os_str().as_encoded_bytes())
    );
    ctx.claude_dir
        .join("cdash-summaries")
        .join(format!("{key}.json"))
}

async fn saved(path: &std::path::Path) -> Option<String> {
    let value: Value = serde_json::from_str(&tokio::fs::read_to_string(path).await.ok()?).ok()?;
    let summary = value.get("summary")?.as_str()?;
    (value.get("version")?.as_u64()? == 1 && (80..=2000).contains(&summary.len()))
        .then(|| summary.to_owned())
}

impl SummaryCache {
    pub async fn attach(ctx: &Arc<Ctx>, sessions: &mut [Resumable]) {
        let can_generate = crate::host::probe::has_binary(&ctx.host.path, "node");
        for session in sessions {
            let Some(source) = session.source_file.as_ref().filter(|p| p.is_absolute()) else {
                continue;
            };
            let Ok(meta) = tokio::fs::metadata(source).await else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let observed = (modified, meta.len());
            let cache = cache_file(ctx, source);
            let missing = !ctx
                .summaries
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entries
                .contains_key(source);
            let existing = if missing { saved(&cache).await } else { None };
            let mut state = ctx.summaries.0.lock().unwrap_or_else(|e| e.into_inner());
            let has_room = state.queue.len() < super::sessions::RESUMABLE_CEILING;
            let entry = state
                .entries
                .entry(source.clone())
                .or_insert_with(|| Entry {
                    observed,
                    summary: existing,
                    busy: false,
                    failed: true,
                    retry_at: Instant::now(),
                });
            session.summary = entry.summary.clone();
            if can_generate
                && has_room
                && !entry.busy
                && (entry.observed != observed
                    || (entry.failed && Instant::now() >= entry.retry_at))
            {
                entry.busy = true;
                entry.observed = observed;
                state.queue.push_back(Job {
                    source: source.clone(),
                    cache,
                });
            }
        }
        let mut state = ctx.summaries.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.running && !state.queue.is_empty() {
            state.running = true;
            tokio::spawn(worker(Arc::clone(ctx)));
        }
    }
}

async fn worker(ctx: Arc<Ctx>) {
    let root = ctx.claude_dir.join("cdash-summaries");
    let initialized = async {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::create_dir_all(&root).await?;
        tokio::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).await?;
        tokio::fs::write(root.join("bridge.mjs"), include_str!("codex-remote.mjs")).await?;
        tokio::fs::write(root.join("summary.mjs"), include_str!("summary.mjs")).await
    }
    .await
    .is_ok();
    loop {
        let job = {
            let mut state = ctx.summaries.0.lock().unwrap_or_else(|e| e.into_inner());
            match state.queue.pop_front() {
                Some(job) => job,
                None => {
                    state.running = false;
                    return;
                }
            }
        };
        let summary = if initialized {
            let payload =
                json!({ "source": job.source, "cache": job.cache, "claudeDir": ctx.claude_dir })
                    .to_string();
            let result = ctx
                .runner
                .run_checked_with_timeout(
                    "node",
                    &[
                        &root.join("bridge.mjs").to_string_lossy(),
                        "--summary",
                        &payload,
                    ],
                    "session summary",
                    Duration::from_secs(300),
                )
                .await;
            result
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.get("summary").and_then(Value::as_str).map(str::to_owned))
        } else {
            None
        };
        let mut state = ctx.summaries.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = state.entries.get_mut(&job.source) {
            entry.busy = false;
            entry.failed = summary.is_none();
            entry.retry_at = Instant::now() + Duration::from_secs(300);
            if summary.is_some() {
                entry.summary = summary;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{init::Host, log::LogBuffer, sample::Sampler};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn polling_deduplicates_work_preserves_preview_on_failure_and_reloads_disk_cache() {
        let dir = std::env::temp_dir().join(format!("cdash-summary-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let node = dir.join("bin/node");
        std::fs::write(&node, format!("#!/bin/sh\necho run >> '{}/calls'\nsleep 0.2\necho '{{\"error\":\"unavailable\"}}'\n", dir.display())).unwrap();
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o700)).unwrap();
        let make_ctx = || {
            Arc::new(Ctx::new(
                Host {
                    path: format!("{}:/usr/bin:/bin", dir.join("bin").display()),
                    runner: crate::host::cmd::Runner::new(
                        format!("{}:/usr/bin:/bin", dir.join("bin").display()),
                        Arc::new(LogBuffer::new()),
                    ),
                    log: Arc::new(LogBuffer::new()),
                    sampler: Mutex::new(Sampler::new()),
                },
                dir.clone(),
                None,
            ))
        };
        let ctx = make_ctx();
        let source = dir.join("session.jsonl");
        std::fs::write(&source, "history").unwrap();
        let cache = cache_file(&ctx, &source);
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        let preview = "A previous summary remains readable when the provider fails. ".repeat(3);
        std::fs::write(
            &cache,
            json!({ "version": 1, "summary": preview }).to_string(),
        )
        .unwrap();
        let mut sessions = vec![Resumable {
            display_name: None,
            sid: "test".into(),
            dir: None,
            ts: 0.,
            branch: None,
            title: "Title".into(),
            prompts: vec![],
            model: None,
            effort: None,
            model_id: None,
            source_file: Some(source),
            summary: None,
        }];
        for _ in 0..5 {
            SummaryCache::attach(&ctx, &mut sessions).await;
        }
        assert_eq!(sessions[0].summary.as_deref(), Some(preview.as_str()));
        tokio::time::sleep(Duration::from_millis(500)).await;
        SummaryCache::attach(&ctx, &mut sessions).await;
        assert_eq!(
            std::fs::read_to_string(dir.join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(sessions[0].summary.as_deref(), Some(preview.as_str()));
        let restarted = make_ctx();
        SummaryCache::attach(&restarted, &mut sessions).await;
        assert_eq!(sessions[0].summary.as_deref(), Some(preview.as_str()));
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}

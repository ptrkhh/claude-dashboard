use crate::host::cmd::Runner;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Mirrors `GIT_TTL_MS` (`lib/collect.js:34`).
pub const GIT_TTL_MS: u64 = 15_000;
/// The 20 s ceiling from `lib/collect.js:41`, deliberately far above the 5 s
/// default: slower than this and the repository simply gets no git badge.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Default)]
struct Entry {
    out: Option<String>,
    ts: u64,
    busy: bool,
}

/// Pure refresh predicate, split out so the two rules that matter can be
/// tested without a repository: stale entries refresh, busy entries never do.
pub fn refresh_due(entry_ts_ms: u64, busy: bool, now_ms: u64) -> bool {
    !busy && now_ms.saturating_sub(entry_ts_ms) > GIT_TTL_MS
}

/// `git status` per directory, refreshed in the background. A poll never waits
/// on git: it gets the last known answer (or `None` the first time) and moves
/// on. Mirrors `gitStatusFor` (`lib/collect.js:35-46`).
pub struct GitCache {
    map: Mutex<HashMap<String, Entry>>,
}

impl Default for GitCache {
    fn default() -> Self {
        Self::new()
    }
}

impl GitCache {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    pub fn status_for(
        self: &Arc<Self>,
        runner: Arc<Runner>,
        dir: &str,
        now_ms: u64,
    ) -> Option<String> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let hit = map.entry(dir.to_string()).or_default();
        let known = hit.out.clone();
        if !refresh_due(hit.ts, hit.busy, now_ms) {
            return known;
        }
        hit.busy = true;
        drop(map);

        let cache = Arc::clone(self);
        let dir_owned = dir.to_string();
        tokio::spawn(async move {
            let out = runner
                .run_with_timeout(
                    "git",
                    &["-C", &dir_owned, "status", "--porcelain=v1", "-b"],
                    // The explicit key: `git <dir>`, so two failing repositories
                    // produce two log lines rather than collapsing into one.
                    &format!("git {dir_owned}"),
                    GIT_TIMEOUT,
                )
                .await;
            let mut map = cache.map.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(
                dir_owned,
                Entry {
                    out: if out.is_empty() { None } else { Some(out) },
                    ts: now_ms,
                    busy: false,
                },
            );
        });

        known
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn runner() -> Arc<Runner> {
        let log = Arc::new(LogBuffer::new());
        Arc::new(Runner::new(std::env::var("PATH").unwrap_or_default(), log))
    }

    #[test]
    fn a_fresh_entry_is_not_refreshed() {
        assert!(!refresh_due(10_000, false, 10_000 + GIT_TTL_MS));
    }

    #[test]
    fn a_stale_entry_is_refreshed() {
        assert!(refresh_due(10_000, false, 10_000 + GIT_TTL_MS + 1));
    }

    #[test]
    fn a_busy_entry_is_never_refreshed_however_stale() {
        // D5: without this, every 4s poll stacks another `git status` on a
        // repository that is already slow enough to still be running.
        assert!(!refresh_due(0, true, 10_000_000));
    }

    #[tokio::test]
    async fn the_first_call_returns_none_immediately_and_does_not_block() {
        let cache = Arc::new(GitCache::new());
        let started = std::time::Instant::now();
        let out = cache.status_for(runner(), "/tmp", 1_000_000);
        assert_eq!(out, None, "a cold entry serves None rather than waiting");
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_real_repository_is_populated_by_the_background_refresh() {
        let dir = std::env::temp_dir().join(format!("cdash-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let r = runner();
        let d = dir.to_str().unwrap();
        r.run("git", &["-C", d, "init", "-q"], "git-init").await;

        let cache = Arc::new(GitCache::new());
        assert_eq!(cache.status_for(r.clone(), d, 1_000_000), None);

        let mut got = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(out) = cache.status_for(r.clone(), d, 1_000_000) {
                got = Some(out);
                break;
            }
        }
        let out = got.expect("the background refresh must eventually populate the entry");
        assert!(out.starts_with("## "), "porcelain -b output starts with the branch header");
    }
}

use super::cache::TranscriptCache;
use super::git::GitCache;
use super::usage::UsageCache;
use crate::host::cmd::Runner;
use crate::host::init::Host;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// What the dashboard knows about a session it launched itself. Mirrors the
/// values Node stored in `ctx.meta` (`lib/collect.js:137,149,234`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub rc_link: Option<String>,
}

/// The shared context every collect entry point takes. Mirrors the `ctx`
/// object built in `server.js:20-26`, plus the two caches and the `Runner`,
/// which in Node were module-level globals.
pub struct Ctx {
    pub host: Host,
    pub runner: Arc<Runner>,
    pub claude_dir: PathBuf,
    /// A second mount to report alongside `/`, e.g. `/mnt/d` (`server.js:22`).
    pub disk_extra: Option<String>,
    pub places_file: PathBuf,
    pub meta: Mutex<HashMap<String, Meta>>,
    pub purged: Mutex<HashSet<String>>,
    pub transcripts: TranscriptCache,
    pub git: Arc<GitCache>,
    /// Claude subscription limits, refreshed off the poll path.
    pub usage: Arc<UsageCache>,
    /// Set once at boot when `CDASH_AUTH` includes `password`. A `OnceLock`
    /// because `Ctx` is shared behind an `Arc` by the time the policy exists.
    pub password: std::sync::OnceLock<crate::auth::login::PasswordState>,
}

impl Ctx {
    pub fn new(host: Host, claude_dir: PathBuf, disk_extra: Option<String>) -> Self {
        // `Host` owns a `Runner` too, but not behind an `Arc`, and the git
        // cache's background task needs one. Same resolved PATH, same log
        // buffer; only the log-once set is per-runner.
        let runner = Arc::new(Runner::new(host.path.clone(), Arc::clone(&host.log)));
        Self {
            places_file: claude_dir.join("cdash-places.json"),
            host,
            runner,
            claude_dir,
            disk_extra,
            meta: Mutex::new(HashMap::new()),
            purged: Mutex::new(HashSet::new()),
            transcripts: TranscriptCache::new(),
            git: Arc::new(GitCache::new()),
            usage: Arc::new(UsageCache::new()),
            password: std::sync::OnceLock::new(),
        }
    }

    pub fn meta_get(&self, name: &str) -> Option<Meta> {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// Read-modify-write under a single lock. `meta_get` + `meta_set` is not a
    /// guard on a multi-threaded runtime: a `POST /api/kill` landing in the gap
    /// is re-inserted by the write.
    pub fn meta_update(&self, name: &str, f: impl FnOnce(&mut Meta)) -> bool {
        let mut g = self.meta.lock().unwrap_or_else(|e| e.into_inner());
        match g.get_mut(name) {
            Some(m) => {
                f(m);
                true
            }
            None => false,
        }
    }

    pub fn meta_set(&self, name: &str, m: Meta) {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), m);
    }

    pub fn meta_has(&self, name: &str) -> bool {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).contains_key(name)
    }

    pub fn meta_delete(&self, name: &str) {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
    }
}

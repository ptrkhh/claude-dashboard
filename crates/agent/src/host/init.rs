use super::cmd::Runner;
use super::log::LogBuffer;
use super::path::probe_path;
use super::probe::missing_binaries;
use super::sample::Sampler;
use std::sync::{Arc, Mutex};

pub struct Host {
    pub runner: Runner,
    pub log: Arc<LogBuffer>,
    pub path: String,
    pub sampler: Mutex<Sampler>,
}

impl Host {
    /// Re-probes every call. Deliberately not cached: the macOS setup screen's
    /// re-check button is worthless against a boot-time answer.
    pub fn missing(&self) -> Vec<String> {
        missing_binaries(&self.path)
    }
}

pub async fn init() -> Host {
    let log = Arc::new(LogBuffer::new());
    let path = probe_path(&log).await;
    Host {
        runner: Runner::new(path.clone(), log.clone()),
        log,
        path,
        sampler: Mutex::new(Sampler::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn init_produces_a_usable_host() {
        let host = init().await;
        assert!(!host.path.is_empty());
        assert_eq!(host.runner.run("echo", &["ok"], "echo").await.trim(), "ok");
    }

    #[tokio::test]
    async fn missing_is_recomputed_on_each_call_not_cached() {
        // UX-5: a user who installs tmux while the agent runs and presses
        // re-check must get the new answer.
        let host = init().await;
        let a = host.missing();
        let b = host.missing();
        assert_eq!(a, b);
        assert!(a.iter().all(|m| super::super::probe::REQUIRED_BINARIES.contains(&m.as_str())));
    }
}

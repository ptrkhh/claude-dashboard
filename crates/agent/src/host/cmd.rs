use super::log::LogBuffer;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default subprocess deadline. This exists because `git status` on a 9P mount
/// once took over 60 seconds and stalled every 4-second poll. Do not raise it
/// without measuring; do not remove it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// The only sanctioned way to run a subprocess. `clippy.toml` forbids
/// `std::process::Command` and `tokio::process::Command` everywhere else, and
/// `-D clippy::disallowed_types` is a required CI gate, because this helper is
/// the sole enforcement of the time-box above.
pub struct Runner {
    path: String,
    log: Arc<LogBuffer>,
    failed: Mutex<HashSet<String>>,
}

impl Runner {
    pub fn new(path: String, log: Arc<LogBuffer>) -> Self {
        Self { path, log, failed: Mutex::new(HashSet::new()) }
    }

    /// Swallowing: failure is an empty string. Correct for the 4-second poll,
    /// where a broken `git status` must not fail the whole request — and wrong
    /// for anything that changes state, which is what `run_checked` is for.
    pub async fn run(&self, program: &str, args: &[&str], key: &str) -> String {
        self.run_with_timeout(program, args, key, DEFAULT_TIMEOUT).await
    }

    pub async fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        key: &str,
        timeout: Duration,
    ) -> String {
        self.run_checked_with_timeout(program, args, key, timeout).await.unwrap_or_default()
    }

    /// Fallible: the caller learns the command failed. Every mutating route
    /// uses this, because reporting a kill that did not happen is worse than
    /// reporting an error.
    pub async fn run_checked(
        &self,
        program: &str,
        args: &[&str],
        key: &str,
    ) -> Result<String, String> {
        self.run_checked_with_timeout(program, args, key, DEFAULT_TIMEOUT).await
    }

    async fn run_checked_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        key: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        #[allow(clippy::disallowed_types)]
        let fut = tokio::process::Command::new(program)
            .args(args)
            .env("PATH", &self.path)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true) // the timeout must actually kill the child
            .output();

        let reason = match tokio::time::timeout(timeout, fut).await {
            Err(_) => format!("timed out after {}ms", timeout.as_millis()),
            Ok(Err(e)) => e.to_string(),
            Ok(Ok(out)) if !out.status.success() => {
                format!("exit {}", out.status.code().unwrap_or(-1))
            }
            Ok(Ok(out)) => return Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        };
        self.log_once(key, &reason);
        Err(format!("{key}: {reason}"))
    }

    /// Log a given failing key once per process lifetime. The KEY IS EXPLICIT:
    /// deriving it from `program + args[0]` is what made every `git status`
    /// failure across every repository collapse into one silenced entry.
    fn log_once(&self, key: &str, reason: &str) {
        let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if failed.insert(key.to_string()) {
            self.log.push(format!("sh failed: {key}: {reason}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> (Runner, Arc<LogBuffer>) {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        (Runner::new(path, log.clone()), log)
    }

    #[tokio::test]
    async fn returns_stdout_on_success() {
        let (r, _) = runner();
        let out = r.run("echo", &["hello"], "echo").await;
        assert_eq!(out.trim(), "hello");
    }

    #[tokio::test]
    async fn returns_empty_string_on_failure() {
        let (r, _) = runner();
        assert_eq!(r.run("false", &[], "false").await, "");
    }

    #[tokio::test]
    async fn logs_once_per_key_not_once_per_failure() {
        let (r, log) = runner();
        for _ in 0..3 {
            r.run("false", &[], "git /repo-a").await;
        }
        assert_eq!(log.lines().len(), 1);
    }

    #[tokio::test]
    async fn distinct_keys_log_separately() {
        // The defect this closes: under the old `cmd + args[0]` key both of
        // these collapsed to "git -C" and the second was silenced.
        let (r, log) = runner();
        r.run("false", &["-C", "/repo-a", "status"], "git /repo-a").await;
        r.run("false", &["-C", "/repo-b", "status"], "git /repo-b").await;
        assert_eq!(log.lines().len(), 2);
    }

    #[tokio::test]
    async fn a_hung_child_is_killed_at_the_timeout() {
        let (r, log) = runner();
        let started = std::time::Instant::now();
        let out = r
            .run_with_timeout("sleep", &["30"], "sleep", Duration::from_millis(300))
            .await;
        assert_eq!(out, "");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(log.lines()[0].contains("timed out"));
    }

    #[tokio::test]
    async fn the_resolved_path_reaches_the_child() {
        let log = Arc::new(LogBuffer::new());
        let r = Runner::new("/nonexistent-dir-for-test".to_string(), log);
        // `echo` is not on the supplied PATH, so the spawn must fail.
        assert_eq!(r.run("echo", &["hi"], "echo").await, "");
    }
}

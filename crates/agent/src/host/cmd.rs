use super::log::LogBuffer;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How much of a child's last stderr line is kept. It reaches the boot log
/// and the body of a 500, and a child can write as much as it likes.
const MAX_STDERR: usize = 200;

/// Default subprocess deadline. This exists because `git status` on a 9P mount
/// once took over 60 seconds and stalled every 4-second poll. Do not raise it
/// without measuring; do not remove it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// `CREATE_NO_WINDOW`: a console child of a windowless parent must not open a
/// console of its own. Ignored when combined with `CREATE_NEW_CONSOLE`.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `CREATE_NEW_CONSOLE`: the one spawn that wants a window — a Claude session.
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x10;

/// The only sanctioned way to run a subprocess. `clippy.toml` forbids
/// `std::process::Command` and `tokio::process::Command` everywhere else, and
/// `-D clippy::disallowed_types` is a required CI gate, because this helper is
/// the sole enforcement of the time-box above.
pub struct Runner {
    path: String,
    log: Arc<LogBuffer>,
    failed: Mutex<HashSet<String>>,
    /// Prepended to every command: `["wsl.exe", "--exec", "/usr/bin/env",
    /// "PATH=…", "LC_ALL=C"]` turns this runner into the WSL side's. Empty for
    /// native.
    prefix: Vec<String>,
    /// Prepended to failure log lines so two sides sharing one log are told
    /// apart: `"wsl "` or `""`.
    label: &'static str,
}

/// Truncate on a character boundary, never a byte one: a child that prints
/// UTF-8 must not have a codepoint sliced in half on its way into the log.
fn cap(s: &str) -> String {
    match s.char_indices().nth(MAX_STDERR) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

impl Runner {
    pub fn new(path: String, log: Arc<LogBuffer>) -> Self {
        Self::with_prefix(Vec::new(), "", path, log)
    }

    pub fn with_prefix(
        prefix: Vec<String>,
        label: &'static str,
        path: String,
        log: Arc<LogBuffer>,
    ) -> Self {
        Self { path, log, failed: Mutex::new(HashSet::new()), prefix, label }
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

    /// The prefix applied: `(program, args)` becomes
    /// `(prefix[0], prefix[1..] ++ [program] ++ args)`.
    fn compose<'a>(&'a self, program: &'a str, args: &[&'a str]) -> (&'a str, Vec<&'a str>) {
        match self.prefix.first() {
            None => (program, args.to_vec()),
            Some(head) => {
                let mut all: Vec<&str> = self.prefix[1..].iter().map(String::as_str).collect();
                all.push(program);
                all.extend_from_slice(args);
                (head.as_str(), all)
            }
        }
    }

    pub async fn run_checked_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        key: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let (program, args) = self.compose(program, args);
        #[allow(clippy::disallowed_types)]
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&args)
            .env("PATH", &self.path)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true); // the timeout must actually kill the child
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let fut = cmd.output();

        let reason = match tokio::time::timeout(timeout, fut).await {
            Err(_) => format!("timed out after {}ms", timeout.as_millis()),
            Ok(Err(e)) => e.to_string(),
            Ok(Ok(out)) if !out.status.success() => {
                let code = out.status.code().unwrap_or(-1);
                // wsl.exe's own messages are UTF-16; dropping the NULs leaves
                // readable ASCII rather than "E R R O R".
                let err = String::from_utf8_lossy(&out.stderr).replace('\0', "");
                match err.lines().rev().find(|l| !l.trim().is_empty()) {
                    Some(last) => format!("exit {code}: {}", cap(last.trim())),
                    None => format!("exit {code}"),
                }
            }
            Ok(Ok(out)) => return Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        };
        self.log_once(key, &reason);
        Err(format!("{key}: {reason}"))
    }

    /// Start a program and do not wait for it: a Claude session in its own
    /// console window. No time-box applies because nothing is awaited; no
    /// `kill_on_drop`, because the session must outlive this process. On
    /// Windows the child gets a new console; from a windowless parent it also
    /// gets that console's standard handles (see spec §3 for the console
    /// parent's limitation). Must be called from within the tokio runtime.
    pub fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        cwd: &str,
        key: &str,
    ) -> Result<(), String> {
        let (program, args) = self.compose(program, args);
        #[allow(clippy::disallowed_types)]
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&args).current_dir(cwd).env("PATH", &self.path);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NEW_CONSOLE);
        match cmd.spawn() {
            Ok(_child) => Ok(()), // dropped: tokio reaps it, it keeps running
            Err(e) => {
                let reason = e.to_string();
                self.log_once(key, &reason);
                Err(format!("{key}: {reason}"))
            }
        }
    }

    /// Drop the one `log_once` line this key would produce, for a caller that
    /// logs a better line itself — `probe_wsl`, whose own line names the
    /// consequence ("Windows side only"). Every other key is untouched.
    pub fn silence(&self, key: &str) {
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string());
    }

    /// Log a given failing key once per process lifetime. The KEY IS EXPLICIT:
    /// deriving it from `program + args[0]` is what made every `git status`
    /// failure across every repository collapse into one silenced entry.
    fn log_once(&self, key: &str, reason: &str) {
        let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if failed.insert(key.to_string()) {
            self.log.push(format!("sh failed: {}{key}: {reason}", self.label));
        }
    }
}

#[cfg(unix)]
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

    #[tokio::test]
    async fn a_prefix_wraps_every_command() {
        // The WSL runner is `wsl.exe --exec env PATH=… <program> <args>`.
        // `env` stands in for wsl.exe here, so the composition is proven
        // without a distro: a variable set by the prefix reaches the child.
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let r = Runner::with_prefix(
            vec!["env".into(), "CDASH_PREFIX_TEST=42".into()],
            "wsl ",
            path,
            log,
        );
        let out = r.run("sh", &["-c", "echo $CDASH_PREFIX_TEST"], "sh").await;
        assert_eq!(out.trim(), "42");
    }

    #[tokio::test]
    async fn the_label_prefixes_the_failure_line() {
        // Two sides share one log; a failed `tmux list-panes` must say which.
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let r = Runner::with_prefix(Vec::new(), "wsl ", path, log.clone());
        r.run("false", &[], "tmux list-panes").await;
        assert!(
            log.lines()[0].contains("sh failed: wsl tmux list-panes"),
            "{:?}",
            log.lines()
        );
    }

    #[tokio::test]
    async fn stderr_reaches_the_error_message() {
        // `schtasks` and `wsl.exe` explain themselves on stderr; "exit 1"
        // alone sends the operator to guess.
        let (r, _) = runner();
        let e = r
            .run_checked("sh", &["-c", "echo boom >&2; exit 7"], "sh")
            .await
            .unwrap_err();
        assert!(e.contains("exit 7: boom"), "{e}");
    }

    #[tokio::test]
    async fn a_silenced_key_logs_nothing_and_leaves_other_keys_alone() {
        // probe_wsl pushes its own "Windows side only" line; the spec allows
        // one line for that failure, not two.
        let (r, log) = runner();
        r.silence("wsl probe");
        assert!(r.run_checked("false", &[], "wsl probe").await.is_err());
        assert!(log.lines().is_empty(), "{:?}", log.lines());
        r.run("false", &[], "git /repo-a").await;
        assert_eq!(log.lines().len(), 1);
    }

    #[tokio::test]
    async fn a_long_stderr_line_is_capped_on_a_character_boundary() {
        let (r, _) = runner();
        // Multi-byte, so a byte-wise cut would split a codepoint and panic.
        let e = r
            .run_checked("sh", &["-c", "printf 'é%.0s' $(seq 400) >&2; exit 1"], "sh")
            .await
            .unwrap_err();
        assert!(e.ends_with('…'), "{e}");
        assert_eq!(e.chars().filter(|c| *c == 'é').count(), MAX_STDERR);
    }

    #[tokio::test]
    async fn spawn_detached_returns_at_once_and_reports_a_missing_program() {
        let (r, _) = runner();
        let started = std::time::Instant::now();
        r.spawn_detached("sleep", &["3"], "/", "sleep").unwrap();
        assert!(started.elapsed() < Duration::from_secs(1), "must not wait for the child");
        assert!(r.spawn_detached("cdash-no-such-program", &[], "/", "nope").is_err());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[tokio::test]
    async fn returns_stdout_on_success_through_cmd() {
        let log = Arc::new(LogBuffer::new());
        let r = Runner::new(std::env::var("PATH").unwrap_or_default(), log);
        let out = r.run("cmd", &["/c", "echo hello"], "cmd").await;
        assert_eq!(out.trim(), "hello");
    }
}

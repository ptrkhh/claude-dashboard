//! WSL stops a distro 15 s after its last Windows-side client (`wsl.exe`)
//! disconnects (`.wslconfig` `[general] instanceIdleTimeout`, default 15000).
//! A detached tmux session is not a client, and neither is a dashboard talking
//! to this agent over localhost, so closing the last terminal powered the
//! distro off and took every cdash session with it ("The system will power off
//! now!" in the pane). While any `cdash-*` session exists, the agent holds one
//! `wsl.exe` client of its own, launched back into this distro through interop,
//! and releases it when the last session ends so an idle distro still stops.

use super::cmd::Runner;
use crate::parse::tmux::{parse_tmux_panes, PANE_FORMAT};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

const WSL_EXE: &str = "/mnt/c/Windows/System32/wsl.exe";

// ponytail: polled, so a new session goes unprotected for up to one tick, well
// inside the 15 s default. Hook spawn/kill instead if that timeout ever drops.
const TICK: Duration = Duration::from_secs(5);

/// One client, held or not.
pub struct KeepAlive {
    program: String,
    args: Vec<String>,
    /// Exists while the client is wanted. Signals do not cross interop: SIGTERM
    /// or SIGKILL to the Linux-side relay leaves the Windows `wsl.exe` running
    /// (seen end to end on WSL 2.7.12), so the client polls this file and
    /// leaves on its own once it is gone.
    lease: PathBuf,
    child: Option<tokio::process::Child>,
}

impl KeepAlive {
    pub fn new(program: &str, args: &[&str], lease: PathBuf) -> Self {
        let args = args.iter().map(|a| a.to_string()).collect();
        Self { program: program.to_string(), args, lease, child: None }
    }

    /// Hold the client while `want`, replacing one that died; release it when not.
    pub async fn reconcile(&mut self, want: bool) {
        if let Some(c) = &mut self.child {
            if !matches!(c.try_wait(), Ok(None)) {
                self.child = None;
            }
        }
        if !want {
            let _ = std::fs::remove_file(&self.lease);
            // Only the relay: its Windows side is already leaving via the lease.
            if let Some(mut c) = self.child.take() {
                let _ = c.kill().await;
            }
            return;
        }
        // ponytail: a relay can outlive its Windows side, so a client that dies
        // on Windows alone goes unnoticed until the agent restarts. Track the
        // inner shell's pid if that is ever seen outside a WSL shutdown.
        if self.child.is_none() && std::fs::write(&self.lease, b"").is_ok() {
            self.child = self.start().ok();
        }
    }

    #[allow(clippy::disallowed_types)] // held open for hours, which Runner (output, 5 s deadline) cannot express
    fn start(&self) -> std::io::Result<tokio::process::Child> {
        tokio::process::Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
    }
}

/// What the client runs inside the distro. It leaves when the lease is dropped,
/// or once `pid` (the agent) is gone however that happened, so a SIGKILLed
/// agent cannot leave a client pinning the distro forever.
fn watch(pid: u32, lease: &Path) -> String {
    let lease = lease.display();
    format!("while kill -0 {pid} 2>/dev/null && [ -e '{lease}' ]; do sleep 1; done")
}

/// A no-op outside WSL. `WSL_DISTRO_NAME` is set for every process WSL starts,
/// and it names the distro to hold, so the client never wakes a different one.
pub fn spawn(runner: Arc<Runner>) {
    let Some(distro) = std::env::var("WSL_DISTRO_NAME").ok().filter(|d| !d.is_empty()) else {
        return;
    };
    let program = if Path::new(WSL_EXE).is_file() { WSL_EXE } else { "wsl.exe" };
    let lease = std::env::temp_dir().join(format!("cdash-keepalive-{}", std::process::id()));
    let script = watch(std::process::id(), &lease);
    let mut held = KeepAlive::new(program, &["-d", &distro, "-e", "sh", "-c", &script], lease);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        loop {
            tick.tick().await;
            let out = runner
                .run("tmux", &["list-panes", "-a", "-F", PANE_FORMAT], "tmux list-panes")
                .await;
            held.reconcile(!parse_tmux_panes(&out).is_empty()).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn pid(k: &KeepAlive) -> Option<u32> {
        k.child.as_ref().and_then(|c| c.id())
    }

    fn alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    fn lease(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cdash-keepalive-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[tokio::test]
    async fn holds_exactly_one_client_while_a_session_exists() {
        let l = lease("hold");
        let mut k = KeepAlive::new("sleep", &["30"], l.clone());
        k.reconcile(true).await;
        let first = pid(&k).expect("client spawned");
        k.reconcile(true).await;
        assert_eq!(pid(&k), Some(first), "a second tick must not spawn a second client");
        assert!(alive(first));
        assert!(l.exists(), "a held client needs its lease");
        k.reconcile(false).await;
    }

    #[tokio::test]
    async fn releases_the_client_once_no_session_remains() {
        let l = lease("release");
        let mut k = KeepAlive::new("sleep", &["30"], l.clone());
        k.reconcile(true).await;
        let held = pid(&k).expect("client spawned");
        k.reconcile(false).await;
        assert_eq!(pid(&k), None);
        assert!(!l.exists(), "the lease is how the Windows side learns to leave");
        assert!(!alive(held));
    }

    #[tokio::test]
    async fn replaces_a_client_that_exited() {
        let mut k = KeepAlive::new("true", &[], lease("replace"));
        k.reconcile(true).await;
        let first = pid(&k).expect("client spawned");
        tokio::time::sleep(Duration::from_millis(300)).await;
        k.reconcile(true).await;
        let second = pid(&k).expect("a dead client is replaced, not trusted");
        assert_ne!(first, second);
        k.reconcile(false).await;
    }

    async fn run_watch(pid: u32, lease: &Path, deadline: Duration) -> Result<String, String> {
        let path = std::env::var("PATH").unwrap_or_default();
        let r = Runner::new(path, Arc::new(LogBuffer::new()));
        r.run_checked_with_timeout("sh", &["-c", &watch(pid, lease)], "keepalive watch", deadline).await
    }

    #[tokio::test]
    async fn the_client_stays_while_held() {
        let l = lease("stay");
        std::fs::write(&l, b"").unwrap();
        let r = run_watch(std::process::id(), &l, Duration::from_millis(2500)).await;
        let _ = std::fs::remove_file(&l);
        assert!(r.is_err(), "client quit while still held");
    }

    /// Release: signals sent to the relay never reach the Windows side, so the
    /// client must leave on its own once the lease is gone.
    #[tokio::test]
    async fn the_client_leaves_once_its_lease_is_dropped() {
        let l = lease("drop");
        std::fs::write(&l, b"").unwrap();
        let dropper = l.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = std::fs::remove_file(dropper);
        });
        assert!(run_watch(std::process::id(), &l, Duration::from_secs(5)).await.is_ok());
    }

    /// The SIGKILLed agent: nothing is left to drop the lease.
    #[tokio::test]
    async fn the_client_leaves_once_the_agent_is_gone() {
        let l = lease("gone");
        std::fs::write(&l, b"").unwrap();
        let r = run_watch(i32::MAX as u32 - 1, &l, Duration::from_secs(5)).await;
        let _ = std::fs::remove_file(&l);
        assert!(r.is_ok());
    }
}

//! The windowless twin of `cdash-agent`, the binary Task Scheduler runs: no
//! console at logon, no subcommands, the same server. The `python`/`pythonw`
//! idiom. Writes to the missing stdout are discarded by Rust's stdio; the log
//! is read at `/api/logs`.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[tokio::main]
async fn main() {
    cdash_agent::http::serve::serve_from_env().await;
}

/// The target exists on every platform so `cargo build` and `-D warnings`
/// behave the same everywhere; off Windows it does nothing.
#[cfg(not(windows))]
fn main() {}

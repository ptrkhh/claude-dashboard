//! A launcher that leaves SIGCHLD ignored (seen at WSL boot, via `~/.bashrc`)
//! makes the kernel reap every child before tokio can wait on it, so the first
//! spawn — the PATH probe — fails with ECHILD. The agent must not inherit that.
#![allow(clippy::disallowed_types)]

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

#[test]
fn an_ignored_sigchld_does_not_break_the_path_probe() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cdash-agent"));
    cmd.env("PORT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SIG_IGN survives exec, which is exactly how a launcher hands it down.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGCHLD, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();

    // The "listening" line is printed after the probe has run.
    let mut listening = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut listening).unwrap();
    child.kill().unwrap();
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    child.wait().unwrap();

    assert!(listening.starts_with("cdash-agent "), "agent did not start: {listening:?}");
    assert!(!stderr.contains("os error 10"), "PATH probe hit ECHILD: {stderr}");
}

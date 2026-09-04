use super::log::LogBuffer;
#[cfg(not(windows))]
use std::time::Duration;

#[cfg(not(windows))]
const PROBE_TIMEOUT: Duration = Duration::from_millis(2000);

#[cfg(windows)]
pub const PATH_SEP: char = ';';
#[cfg(not(windows))]
pub const PATH_SEP: char = ':';

/// Where an installer puts binaries that a GUI launch's PATH lacks: Homebrew
/// and /usr/local on Unix, the native Claude installer's
/// `%USERPROFILE%\.local\bin` on Windows.
pub fn known_locations() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![home().join(".local").join("bin").to_string_lossy().into_owned()]
    }
    #[cfg(not(windows))]
    {
        vec!["/opt/homebrew/bin".to_string(), "/usr/local/bin".to_string()]
    }
}

/// The user's home. `std::env::home_dir` reads `$HOME` on Unix and the
/// profile directory on Windows, and is not deprecated on the pinned
/// toolchain (verified: `rustc -D deprecated` accepts it on 1.94.1).
pub fn home() -> std::path::PathBuf {
    std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
}

/// Compose the child PATH: probed value first (so a user's own ordering wins),
/// then the known-location backstop, then whatever we inherited. Deduped,
/// first occurrence kept, empty segments dropped.
pub fn compose_path(probed: Option<&str>, inherited: &str) -> String {
    let sep = PATH_SEP.to_string();
    let known = known_locations().join(&sep);
    let mut out: Vec<&str> = Vec::new();
    for src in [probed.unwrap_or(""), known.as_str(), inherited] {
        for seg in src.split(PATH_SEP) {
            if !seg.is_empty() && !out.contains(&seg) {
                out.push(seg);
            }
        }
    }
    out.join(&sep)
}

/// Probe the user's login-shell PATH. Never returns an error and never gates
/// startup: on timeout, spawn failure, or non-zero exit, fall back to the
/// inherited PATH and record why.
pub async fn probe_path(log: &LogBuffer) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();

    // No login shell to ask on Windows: a logon-triggered task inherits the
    // user's own environment block, so the inherited PATH is the user's PATH.
    #[cfg(windows)]
    {
        let _ = log;
        compose_path(None, &inherited)
    }

    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

        // The one deliberate direct use outside host/cmd.rs: the helper in
        // crates/agent/src/host/cmd.rs depends on the value this function
        // produces, so it cannot itself be routed through the helper.
        #[allow(clippy::disallowed_types)]
        let spawned = tokio::process::Command::new(&shell)
            .args(["-l", "-c", "echo $PATH"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();

        let probed = match tokio::time::timeout(PROBE_TIMEOUT, spawned).await {
            Err(_) => {
                log.push("PATH probe failed (timed out after 2000ms); using inherited PATH");
                None
            }
            Ok(Err(e)) => {
                log.push(format!("PATH probe failed ({e}); using inherited PATH"));
                None
            }
            Ok(Ok(out)) if !out.status.success() => {
                log.push(format!(
                    "PATH probe failed (exit {}); using inherited PATH",
                    out.status.code().unwrap_or(-1)
                ));
                None
            }
            Ok(Ok(out)) => Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        };

        compose_path(probed.as_deref(), &inherited)
    }
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probed_path_comes_first_then_known_locations_then_inherited() {
        let out = compose_path(Some("/home/u/.local/bin:/usr/bin"), "/usr/bin:/bin");
        assert_eq!(
            out,
            "/home/u/.local/bin:/usr/bin:/opt/homebrew/bin:/usr/local/bin:/bin"
        );
    }

    #[test]
    fn a_failed_probe_still_gets_the_known_location_backstop() {
        // This is the whole point of the backstop: on a macOS GUI launch the
        // inherited PATH is exactly the minimal one the probe existed to fix.
        let out = compose_path(None, "/usr/bin:/bin");
        assert_eq!(out, "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn duplicates_are_removed_keeping_first_occurrence() {
        let out = compose_path(Some("/usr/bin"), "/usr/bin:/bin:/usr/bin");
        assert_eq!(out, "/usr/bin:/opt/homebrew/bin:/usr/local/bin:/bin");
    }

    #[test]
    fn empty_segments_are_dropped() {
        let out = compose_path(Some(""), "/bin::/usr/bin");
        assert_eq!(out, "/opt/homebrew/bin:/usr/local/bin:/bin:/usr/bin");
    }

    #[tokio::test]
    async fn probe_never_panics_and_always_returns_a_usable_path() {
        let log = LogBuffer::new();
        let p = probe_path(&log).await;
        assert!(!p.is_empty());
        assert!(p.contains("/usr/local/bin"));
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;

    #[test]
    fn compose_path_dedupes_on_the_platform_separator_and_keeps_the_backstop() {
        let sep = PATH_SEP.to_string();
        let a = std::env::temp_dir().join("cdash-a").to_string_lossy().into_owned();
        let b = std::env::temp_dir().join("cdash-b").to_string_lossy().into_owned();
        let out = compose_path(Some(&a), &format!("{a}{sep}{b}{sep}{a}"));
        let segs: Vec<&str> = out.split(PATH_SEP).collect();
        assert_eq!(segs[0], a, "the probed value comes first");
        assert_eq!(segs.iter().filter(|s| **s == a).count(), 1, "deduped");
        assert!(segs.contains(&b.as_str()));
        for k in known_locations() {
            assert!(segs.contains(&k.as_str()), "backstop {k} missing from {out}");
        }
    }
}

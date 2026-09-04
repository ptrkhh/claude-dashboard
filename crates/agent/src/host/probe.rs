use super::path::PATH_SEP;
use std::path::Path;

/// `ps` and `df` are absent by design: the Rust agent uses `sysinfo` and
/// `statvfs` and never shells out to them. tmux is required only where tmux
/// is the session backend; on Windows the WSL side reports its own list
/// through `/api/hostinfo`.
#[cfg(windows)]
pub const REQUIRED_BINARIES: &[&str] = &["claude", "git"];
#[cfg(not(windows))]
pub const REQUIRED_BINARIES: &[&str] = &["tmux", "claude", "git"];

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Only the native installer's `claude.exe` counts: an npm `claude.cmd` is
/// not an executable image and cannot be spawned by CreateProcess.
#[cfg(windows)]
fn is_executable(p: &Path) -> bool {
    p.with_extension("exe").is_file()
}

pub fn missing_binaries(path: &str) -> Vec<String> {
    REQUIRED_BINARIES
        .iter()
        .filter(|bin| {
            !path
                .split(PATH_SEP)
                .filter(|d| !d.is_empty())
                .any(|dir| is_executable(&Path::new(dir).join(bin)))
        })
        .map(|b| b.to_string())
        .collect()
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-probe-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_executable(dir: &std::path::Path, name: &str) {
        let p = dir.join(name);
        fs::write(&p, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn reports_all_required_binaries_when_path_is_empty() {
        let missing = missing_binaries("");
        assert_eq!(missing.iter().map(String::as_str).collect::<Vec<_>>(), REQUIRED_BINARIES);
    }

    #[test]
    fn a_present_executable_is_not_reported_missing() {
        let dir = tempdir("present");
        make_executable(&dir, "tmux");
        let missing = missing_binaries(dir.to_str().unwrap());
        assert!(!missing.contains(&"tmux".to_string()));
        assert!(missing.contains(&"git".to_string()));
    }

    #[test]
    fn a_non_executable_file_still_counts_as_missing() {
        let dir = tempdir("nonexec");
        fs::write(dir.join("git"), "not executable").unwrap();
        assert!(missing_binaries(dir.to_str().unwrap()).contains(&"git".to_string()));
    }

    #[test]
    fn ps_and_df_are_not_required() {
        assert!(!REQUIRED_BINARIES.contains(&"ps"));
        assert!(!REQUIRED_BINARIES.contains(&"df"));
    }

    #[test]
    fn required_binaries_are_exactly_tmux_claude_git_on_unix() {
        assert_eq!(REQUIRED_BINARIES, &["tmux", "claude", "git"]);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn an_exe_file_on_a_semicolon_separated_path_is_found() {
        let dir = std::env::temp_dir().join(format!("cdash-probe-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("git.exe"), "MZ").unwrap();
        let path = format!("C:\\definitely-not-here;{}", dir.display());
        let missing = missing_binaries(&path);
        assert!(!missing.contains(&"git".to_string()), "{missing:?}");
        assert!(missing.contains(&"claude".to_string()));
    }

    #[test]
    fn required_binaries_are_exactly_claude_git_on_windows() {
        assert_eq!(REQUIRED_BINARIES, &["claude", "git"]);
    }
}

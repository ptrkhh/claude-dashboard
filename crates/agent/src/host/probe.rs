use std::path::Path;

/// `ps` and `df` are absent by design: the Rust agent uses `sysinfo` and
/// `statvfs` and never shells out to them.
pub const REQUIRED_BINARIES: &[&str] = &["tmux", "claude", "git"];

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

pub fn missing_binaries(path: &str) -> Vec<String> {
    REQUIRED_BINARIES
        .iter()
        .filter(|bin| {
            !path
                .split(':')
                .filter(|d| !d.is_empty())
                .any(|dir| is_executable(&Path::new(dir).join(bin)))
        })
        .map(|b| b.to_string())
        .collect()
}

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
        assert_eq!(missing, vec!["tmux", "claude", "git"]);
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
}

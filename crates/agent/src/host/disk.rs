use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskUsage {
    pub mount: String,
    #[serde(rename = "freeKb")]
    pub free_kb: u64,
    #[serde(rename = "totalKb")]
    pub total_kb: u64,
}

/// Disk usage for one named mount. The caller supplies the label, so no mount
/// column is parsed and a path containing a space cannot shift the numbers.
pub fn disk_usage(mount: &str) -> Option<DiskUsage> {
    let stat = rustix::fs::statvfs(mount).ok()?;
    let block = stat.f_frsize.max(1);
    // Node reported 1K blocks (`df -k`); keep the same unit.
    let to_kb = |blocks: u64| blocks.saturating_mul(block) / 1024;
    Some(DiskUsage {
        mount: mount.to_string(),
        free_kb: to_kb(stat.f_bavail),
        total_kb: to_kb(stat.f_blocks),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_reports_plausible_totals() {
        let u = disk_usage("/").expect("/ must be statvfs-able");
        assert_eq!(u.mount, "/");
        assert!(u.total_kb > 0);
        assert!(u.free_kb <= u.total_kb);
    }

    #[test]
    fn a_mount_path_containing_a_space_is_not_mangled() {
        // The Node defect: `df` output was split on whitespace, so this path
        // truncated to "/tmp/with" and shifted totalKb into freeKb.
        let dir = std::env::temp_dir().join("cdash disk test");
        std::fs::create_dir_all(&dir).unwrap();
        let u = disk_usage(dir.to_str().unwrap()).expect("temp dir must be statvfs-able");
        assert_eq!(u.mount, dir.to_str().unwrap());
        assert!(u.total_kb > 0);
    }

    #[test]
    fn a_nonexistent_path_yields_none_rather_than_panicking() {
        assert!(disk_usage("/definitely/not/a/real/mount/point").is_none());
    }

    #[test]
    fn serializes_with_nodes_field_names() {
        let u = DiskUsage { mount: "/".into(), free_kb: 1, total_kb: 2 };
        let j = serde_json::to_string(&u).unwrap();
        assert!(j.contains("\"freeKb\":1"));
        assert!(j.contains("\"totalKb\":2"));
    }
}

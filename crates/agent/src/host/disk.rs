use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskUsage {
    pub mount: String,
    #[serde(rename = "freeKb")]
    pub free_kb: u64,
    #[serde(rename = "totalKb")]
    pub total_kb: u64,
}

/// The mount the stats bar always reports: `/` on Unix, the system drive on
/// Windows (`C:\` unless Windows was installed elsewhere).
pub fn root_mount() -> String {
    #[cfg(windows)]
    {
        format!("{}\\", std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string()))
    }
    #[cfg(not(windows))]
    {
        "/".to_string()
    }
}

/// `GetDiskFreeSpaceExW` documents a UNC directory name as needing a trailing
/// backslash: without one `\\wsl.localhost\Ubuntu` fails and the second
/// mount silently vanishes from the stats bar. `DISK_EXTRA` is typed by hand
/// and spec §10 asks for exactly that value to work, so normalise rather than
/// document the trap. Pure, so it is tested on every host.
pub fn unc_dir_arg(mount: &str) -> String {
    if mount.starts_with("\\\\") && !mount.ends_with('\\') {
        format!("{mount}\\")
    } else {
        mount.to_string()
    }
}

/// One `GetDiskFreeSpaceExW` call — the same shape as `statvfs(mount)`: the
/// caller names the directory and nothing is listed or parsed. A mapped drive
/// or a UNC path is answered by the same call; a path that does not exist is
/// `None`. `sysinfo::Disks` was rejected: it opens every fixed and removable
/// volume with `DeviceIoControl` on each poll and skips network drives.
#[cfg(windows)]
pub fn disk_usage(mount: &str) -> Option<DiskUsage> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    // The label stays what the caller named; only the argument is normalised.
    let wide: Vec<u16> =
        unc_dir_arg(mount).encode_utf16().chain(std::iter::once(0)).collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and outlives the call; the out-pointers
    // are to locals; the fourth pointer is documented as optional.
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
    };
    if ok == 0 {
        return None;
    }
    Some(DiskUsage { mount: mount.to_string(), free_kb: free / 1024, total_kb: total / 1024 })
}

/// Disk usage for one named mount. The caller supplies the label, so no mount
/// column is parsed and a path containing a space cannot shift the numbers.
#[cfg(unix)]
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
    fn the_root_mount_reports_plausible_totals() {
        let m = root_mount();
        let u = disk_usage(&m).expect("the root mount must be statable");
        assert_eq!(u.mount, m);
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
    fn a_unc_mount_gains_the_trailing_backslash_the_api_requires() {
        // Spec §10 check #8: DISK_EXTRA=\\wsl.localhost\Ubuntu\ must work,
        // and so must the same value typed without the trailing separator.
        assert_eq!(unc_dir_arg(r"\\wsl.localhost\Ubuntu"), r"\\wsl.localhost\Ubuntu\");
        assert_eq!(unc_dir_arg(r"\\wsl.localhost\Ubuntu\"), r"\\wsl.localhost\Ubuntu\");
        // Nothing else is touched: a drive root and a POSIX mount pass through.
        assert_eq!(unc_dir_arg(r"C:\"), r"C:\");
        assert_eq!(unc_dir_arg("/mnt/d"), "/mnt/d");
    }

    #[test]
    fn serializes_with_nodes_field_names() {
        let u = DiskUsage { mount: "/".into(), free_kb: 1, total_kb: 2 };
        let j = serde_json::to_string(&u).unwrap();
        assert!(j.contains("\"freeKb\":1"));
        assert!(j.contains("\"totalKb\":2"));
    }
}

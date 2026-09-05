use super::validate::BadRequest;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Entries are capped so an enormous directory cannot stall a tap on the
/// folder picker. Mirrors `MAX_ENTRIES` (`lib/browse.js:7`).
pub const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

/// One breadcrumb: what to show and where it navigates. Built here rather
/// than in the client so the client never learns a path separator.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Crumb {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
    pub truncated: bool,
    pub crumbs: Vec<Crumb>,
}

/// A browse failure is always a 400 with a fixed message (the errno mapping in
/// `server.js:47`), which is exactly what `BadRequest` already means.
fn map_err(e: &std::io::Error) -> BadRequest {
    use std::io::ErrorKind;
    let message = match e.kind() {
        ErrorKind::PermissionDenied => "Permission denied",
        ErrorKind::NotFound => "No such folder",
        ErrorKind::NotADirectory => "Not a folder",
        _ => "Cannot read folder",
    };
    BadRequest(message.to_string())
}

/// Crumbs from the components: the prefix and root make the first crumb
/// (`/`, `C:\`, `\\wsl.localhost\Ubuntu\`), every normal component appends
/// one. On Windows a virtual root crumb `/` comes first — the roots listing.
pub fn crumbs_for(abs: &Path) -> Vec<Crumb> {
    use std::path::Component;
    let mut out = Vec::new();
    let mut acc = PathBuf::new();
    if cfg!(windows) {
        out.push(Crumb { name: "/".into(), path: "/".into() });
    }
    for c in abs.components() {
        match c {
            Component::Prefix(p) => {
                acc = PathBuf::from(p.as_os_str());
                acc.push(std::path::MAIN_SEPARATOR_STR);
                let s = acc.to_string_lossy().into_owned();
                out.push(Crumb { name: s.clone(), path: s });
            }
            Component::RootDir => {
                if acc.as_os_str().is_empty() {
                    acc.push(std::path::MAIN_SEPARATOR_STR);
                    let s = acc.to_string_lossy().into_owned();
                    out.push(Crumb { name: s.clone(), path: s });
                }
            }
            Component::Normal(n) => {
                acc.push(n);
                out.push(Crumb {
                    name: n.to_string_lossy().into_owned(),
                    path: acc.to_string_lossy().into_owned(),
                });
            }
            Component::CurDir | Component::ParentDir => {}
        }
    }
    out
}

/// Every drive letter whose root exists, as `X:\`.
#[cfg(windows)]
pub fn drive_roots() -> Vec<String> {
    ('A'..='Z')
        .map(|d| format!("{d}:\\"))
        .filter(|r| Path::new(r).is_dir())
        .collect()
}

/// Folders only — a project directory is what is being chosen — plus symlinks,
/// which commonly point at directories. Sorted case-insensitively. On Windows
/// the path `/` is the roots listing: `roots` are the drives and the WSL share
/// the route supplies, and a drive's parent is `/`.
pub async fn list_dirs(
    target: &str,
    show_hidden: bool,
    roots: &[String],
) -> Result<Listing, BadRequest> {
    if cfg!(windows) && target == "/" {
        return Ok(Listing {
            path: "/".into(),
            parent: None,
            entries: roots.iter().map(|r| DirEntry { name: r.clone(), path: r.clone() }).collect(),
            truncated: false,
            crumbs: vec![Crumb { name: "/".into(), path: "/".into() }],
        });
    }
    let abs = if target.is_empty() {
        PathBuf::from("/")
    } else {
        std::path::absolute(Path::new(target)).unwrap_or_else(|_| PathBuf::from(target))
    };
    let mut rd = tokio::fs::read_dir(&abs).await.map_err(|e| map_err(&e))?;

    let mut names: Vec<String> = Vec::new();
    while let Some(e) = rd.next_entry().await.map_err(|e| map_err(&e))? {
        let Ok(ft) = e.file_type().await else { continue };
        if !ft.is_dir() && !ft.is_symlink() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        names.push(name);
    }

    // ponytail: lowercase compare stands in for localeCompare's
    // `sensitivity: 'base'`; it agrees on case and differs only on accent
    // folding. Swap for a collation crate only if that ever shows up.
    names.sort_by_key(|n| n.to_lowercase());

    let truncated = names.len() > MAX_ENTRIES;
    names.truncate(MAX_ENTRIES);

    let parent = abs
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| if cfg!(windows) { Some("/".to_string()) } else { None });
    Ok(Listing {
        parent,
        entries: names
            .into_iter()
            .map(|name| DirEntry {
                path: abs.join(&name).to_string_lossy().into_owned(),
                name,
            })
            .collect(),
        crumbs: crumbs_for(&abs),
        path: abs.to_string_lossy().into_owned(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("cdash-browse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join("Beta")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("a-file.txt"), "x").unwrap();
        root
    }

    #[test]
    fn crumbs_for_a_unix_path_start_at_the_root() {
        let c = crumbs_for(Path::new("/a/b"));
        let pairs: Vec<(&str, &str)> = c.iter().map(|x| (x.name.as_str(), x.path.as_str())).collect();
        if cfg!(windows) {
            // `/a/b` is not a Windows shape; the drive tests below cover Windows.
            assert_eq!(pairs.first().map(|p| p.0), Some("/"));
        } else {
            assert_eq!(pairs, vec![("/", "/"), ("a", "/a"), ("b", "/a/b")]);
        }
    }

    #[tokio::test]
    async fn a_listing_carries_its_crumbs() {
        let root = fixture("crumbs");
        let d = list_dirs(root.to_str().unwrap(), false, &[]).await.unwrap();
        assert_eq!(d.crumbs.last().unwrap().path, d.path, "the last crumb is the listing itself");
        assert!(d.crumbs.len() >= 2);
    }

    #[tokio::test]
    async fn returns_folders_only_case_insensitively_sorted_hidden_excluded() {
        let root = fixture("basic");
        let d = list_dirs(root.to_str().unwrap(), false, &[]).await.unwrap();
        assert_eq!(
            d.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "Beta"],
            "no file, no dotdir, and 'alpha' sorts before 'Beta'"
        );
        assert_eq!(d.path, root.to_str().unwrap());
        assert_eq!(d.parent.as_deref(), root.parent().unwrap().to_str());
        assert_eq!(d.entries[0].path, root.join("alpha").to_str().unwrap());
        assert!(!d.truncated);
    }

    #[tokio::test]
    async fn includes_dotfolders_when_asked() {
        let root = fixture("hidden");
        let d = list_dirs(root.to_str().unwrap(), true, &[]).await.unwrap();
        assert_eq!(
            d.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec![".hidden", "alpha", "Beta"]
        );
    }

    #[tokio::test]
    async fn reports_a_null_parent_at_the_filesystem_root() {
        let d = list_dirs("/", false, &[]).await.unwrap();
        assert_eq!(d.parent, None);
        assert_eq!(d.path, "/");
    }

    #[tokio::test]
    async fn a_directory_over_the_cap_is_truncated_and_says_so() {
        // B3: Node has no test for this. An enormous directory must not stall
        // a tap on the folder picker.
        let root = std::env::temp_dir().join(format!("cdash-browse-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..(MAX_ENTRIES + 5) {
            std::fs::create_dir(root.join(format!("d{i:05}"))).unwrap();
        }
        let d = list_dirs(root.to_str().unwrap(), false, &[]).await.unwrap();
        assert_eq!(d.entries.len(), MAX_ENTRIES);
        assert!(d.truncated);
    }

    #[tokio::test]
    async fn a_nonexistent_path_yields_a_400_with_a_fixed_message() {
        // A7: the raw OS error must not reach the client.
        let e = list_dirs("/no/such/dir/cdash-xyz", false, &[]).await.unwrap_err();
        // BadRequest is the 400 — the HTTP layer maps it in one place.
        assert_eq!(e, BadRequest("No such folder".into()));
    }

    #[tokio::test]
    async fn a_file_target_yields_the_not_a_folder_message() {
        let root = fixture("notdir");
        let f = root.join("a-file.txt");
        let e = list_dirs(f.to_str().unwrap(), false, &[]).await.unwrap_err();
        assert_eq!(e, BadRequest("Not a folder".into()));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn crumbs_for_a_drive_and_a_share_path_begin_with_the_virtual_root() {
        let c = crumbs_for(Path::new(r"C:\Users\u"));
        let pairs: Vec<(&str, &str)> = c.iter().map(|x| (x.name.as_str(), x.path.as_str())).collect();
        assert_eq!(pairs, vec![("/", "/"), (r"C:\", r"C:\"), ("Users", r"C:\Users"), ("u", r"C:\Users\u")]);

        let c = crumbs_for(Path::new(r"\\wsl.localhost\Ubuntu\home"));
        let pairs: Vec<(&str, &str)> = c.iter().map(|x| (x.name.as_str(), x.path.as_str())).collect();
        assert_eq!(
            pairs,
            vec![("/", "/"), (r"\\wsl.localhost\Ubuntu\", r"\\wsl.localhost\Ubuntu\"), ("home", r"\\wsl.localhost\Ubuntu\home")]
        );
    }

    #[tokio::test]
    async fn the_slash_path_lists_the_given_roots_and_a_drive_root_has_slash_as_parent() {
        let roots = vec![r"C:\".to_string(), r"\\wsl.localhost\Ubuntu\".to_string()];
        let d = list_dirs("/", false, &roots).await.unwrap();
        assert_eq!(d.path, "/");
        assert_eq!(d.parent, None);
        assert_eq!(d.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), roots);
        assert_eq!(d.crumbs.len(), 1);

        let c = list_dirs(r"C:\", false, &roots).await.unwrap();
        assert_eq!(c.parent.as_deref(), Some("/"));
    }
}

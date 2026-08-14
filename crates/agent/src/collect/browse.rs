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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
    pub truncated: bool,
}

/// A browse failure is always a 400 with a fixed message. Mirrors the errno
/// mapping in `server.js:47`, kept next to the function that produces the
/// errors so the HTTP layer cannot forget it.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowseError {
    pub message: String,
}

impl BrowseError {
    pub fn status(&self) -> u16 {
        400
    }
}

fn map_err(e: &std::io::Error) -> BrowseError {
    use std::io::ErrorKind;
    let message = match e.kind() {
        ErrorKind::PermissionDenied => "Permission denied",
        ErrorKind::NotFound => "No such folder",
        ErrorKind::NotADirectory => "Not a folder",
        _ => "Cannot read folder",
    };
    BrowseError { message: message.to_string() }
}

/// Folders only — a project directory is what is being chosen — plus symlinks,
/// which commonly point at directories. Sorted case-insensitively.
pub async fn list_dirs(target: &str, show_hidden: bool) -> Result<Listing, BrowseError> {
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

    Ok(Listing {
        parent: abs.parent().map(|p| p.to_string_lossy().into_owned()),
        entries: names
            .into_iter()
            .map(|name| DirEntry {
                path: abs.join(&name).to_string_lossy().into_owned(),
                name,
            })
            .collect(),
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

    #[tokio::test]
    async fn returns_folders_only_case_insensitively_sorted_hidden_excluded() {
        let root = fixture("basic");
        let d = list_dirs(root.to_str().unwrap(), false).await.unwrap();
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
        let d = list_dirs(root.to_str().unwrap(), true).await.unwrap();
        assert_eq!(
            d.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec![".hidden", "alpha", "Beta"]
        );
    }

    #[tokio::test]
    async fn reports_a_null_parent_at_the_filesystem_root() {
        let d = list_dirs("/", false).await.unwrap();
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
        let d = list_dirs(root.to_str().unwrap(), false).await.unwrap();
        assert_eq!(d.entries.len(), MAX_ENTRIES);
        assert!(d.truncated);
    }

    #[tokio::test]
    async fn a_nonexistent_path_yields_a_400_with_a_fixed_message() {
        // A7: the raw OS error must not reach the client.
        let e = list_dirs("/no/such/dir/cdash-xyz", false).await.unwrap_err();
        assert_eq!(e.message, "No such folder");
        assert_eq!(e.status(), 400);
    }

    #[tokio::test]
    async fn a_file_target_yields_the_not_a_folder_message() {
        let root = fixture("notdir");
        let f = root.join("a-file.txt");
        let e = list_dirs(f.to_str().unwrap(), false).await.unwrap_err();
        assert_eq!(e.message, "Not a folder");
    }
}

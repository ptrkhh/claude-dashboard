use super::fsio::{read_if, write_atomic};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Mirrors `MAX_RECENTS` (`lib/places.js:7`).
pub const MAX_RECENTS: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Places {
    #[serde(default)]
    pub recents: Vec<String>,
    #[serde(default)]
    pub favorites: Vec<String>,
}

pub fn push_recent(list: &[String], p: &str, max: usize) -> Vec<String> {
    let mut out = vec![p.to_string()];
    out.extend(list.iter().filter(|x| x.as_str() != p).cloned());
    out.truncate(max);
    out
}

pub fn toggle_in(list: &[String], p: &str) -> Vec<String> {
    if list.iter().any(|x| x == p) {
        list.iter().filter(|x| x.as_str() != p).cloned().collect()
    } else {
        let mut out = list.to_vec();
        out.push(p.to_string());
        out
    }
}

/// Node returned the empty shape for a missing file, a malformed file, and a
/// file whose fields are the wrong type (`lib/places.js:19-27`).
/// `#[serde(default)]` covers absence; the `unwrap_or_default` covers the rest.
pub async fn read_places(file: &Path) -> Places {
    match read_if(file).await {
        Some(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        None => Places::default(),
    }
}

async fn write_places(file: &Path, data: &Places) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".to_string());
    write_atomic(file, &json, ".tmp").await
}

pub async fn add_recent(file: &Path, p: &str) -> std::io::Result<Places> {
    let mut data = read_places(file).await;
    data.recents = push_recent(&data.recents, p, MAX_RECENTS);
    write_places(file, &data).await?;
    Ok(data)
}

pub async fn toggle_favorite(file: &Path, p: &str) -> std::io::Result<Places> {
    let mut data = read_places(file).await;
    data.favorites = toggle_in(&data.favorites, p);
    write_places(file, &data).await?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn tempfile(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-places-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("places.json")
    }

    #[test]
    fn push_recent_moves_an_existing_entry_to_the_front_without_duplicating() {
        assert_eq!(push_recent(&s(&["/a", "/b", "/c"]), "/c", MAX_RECENTS), s(&["/c", "/a", "/b"]));
        assert_eq!(push_recent(&s(&["/a", "/b"]), "/x", MAX_RECENTS), s(&["/x", "/a", "/b"]));
    }

    #[test]
    fn push_recent_caps_the_list_length() {
        let many: Vec<String> = (0..MAX_RECENTS).map(|i| format!("/p{i}")).collect();
        let out = push_recent(&many, "/new", MAX_RECENTS);
        assert_eq!(out.len(), MAX_RECENTS);
        assert_eq!(out[0], "/new");
        assert!(!out.contains(&format!("/p{}", MAX_RECENTS - 1)), "oldest dropped");
    }

    #[test]
    fn toggle_in_adds_then_removes() {
        assert_eq!(toggle_in(&[], "/a"), s(&["/a"]));
        assert_eq!(toggle_in(&s(&["/a", "/b"]), "/a"), s(&["/b"]));
    }

    #[tokio::test]
    async fn read_places_returns_the_empty_shape_for_a_missing_or_malformed_file() {
        let p = read_places(Path::new("/definitely/not/here.json")).await;
        assert!(p.recents.is_empty() && p.favorites.is_empty());

        let f = tempfile("bad");
        tokio::fs::write(&f, "{\"recents\":\"not an array\"}").await.unwrap();
        let p = read_places(&f).await;
        assert!(p.recents.is_empty(), "a wrongly-typed field falls back to empty, not an error");
    }

    #[tokio::test]
    async fn add_recent_and_toggle_favorite_persist_to_disk() {
        let f = tempfile("persist");
        add_recent(&f, "/home/x/one").await.unwrap();
        add_recent(&f, "/home/x/two").await.unwrap();
        assert_eq!(read_places(&f).await.recents, s(&["/home/x/two", "/home/x/one"]));

        toggle_favorite(&f, "/home/x/one").await.unwrap();
        assert_eq!(read_places(&f).await.favorites, s(&["/home/x/one"]));

        toggle_favorite(&f, "/home/x/one").await.unwrap();
        assert!(read_places(&f).await.favorites.is_empty());
    }

    #[tokio::test]
    async fn the_write_is_atomic_and_leaves_no_temp_file() {
        let f = tempfile("atomic");
        add_recent(&f, "/a").await.unwrap();
        let tmp = f.with_file_name("places.json.tmp");
        assert!(!tmp.exists());
    }
}

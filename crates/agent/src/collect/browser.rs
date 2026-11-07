use super::fsio::read_if;
use std::path::Path;

/// Hand-edited map of absolute session dir to loopback CDP port, in `$CLAUDE_DIR`.
pub const MAP_FILE: &str = "cdash-browsers.json";

/// Missing or malformed file means no browsers. Keys must be absolute (a
/// trailing `/` is trimmed); ports must be integers in `1024..=65535`.
pub async fn read_port_map(claude_dir: &Path) -> Vec<(String, u16)> {
    let Some(txt) = read_if(&claude_dir.join(MAP_FILE)).await else { return Vec::new() };
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_str(&txt) else { return Vec::new() };
    obj.into_iter()
        .filter_map(|(k, v)| {
            if !k.starts_with('/') {
                return None;
            }
            let port = u16::try_from(v.as_u64()?).ok().filter(|p| *p >= 1024)?;
            let key = k.trim_end_matches('/');
            Some((if key.is_empty() { "/" } else { key }.to_string(), port))
        })
        .collect()
}

/// Longest key that equals `dir` or is a parent of it, by path component.
pub fn port_for(map: &[(String, u16)], dir: &str) -> Option<u16> {
    map.iter()
        .filter(|(k, _)| k == "/" || dir == k.as_str() || dir.starts_with(&format!("{k}/")))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, p)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_match_is_by_path_component() {
        let map = vec![("/mnt/d/p/Downloads/JOBS".to_string(), 9301), ("/mnt/d/p".to_string(), 9400)];
        assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS"), Some(9301));
        assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS/sub"), Some(9301));
        assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS2"), Some(9400), "sibling falls to the parent key");
        assert_eq!(port_for(&map, "/mnt/d/q"), None);
    }

    #[tokio::test]
    async fn map_file_rules() {
        let dir = std::env::temp_dir().join(format!("cdash-bmap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_port_map(&dir).await.is_empty(), "missing file");
        std::fs::write(dir.join(MAP_FILE), "{not json").unwrap();
        assert!(read_port_map(&dir).await.is_empty(), "malformed file");
        std::fs::write(dir.join(MAP_FILE), r#"{"/a/":9301,"rel":9302,"/b":80,"/c":70000,"/d":"9303","/e":9304}"#).unwrap();
        let mut m = read_port_map(&dir).await;
        m.sort();
        assert_eq!(m, vec![("/a".to_string(), 9301), ("/e".to_string(), 9304)]);
    }
}

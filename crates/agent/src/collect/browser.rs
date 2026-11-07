use super::fsio::read_if;
use std::path::Path;
use std::time::Duration;

/// Discovery deadline, request plus body.
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);
/// Per-poll deadline for the card's Browser-button probe.
const PROBE_DEADLINE: Duration = Duration::from_millis(300);

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

async fn get_json(port: u16, path: &str) -> Result<serde_json::Value, &'static str> {
    // No proxy and no redirects: a 3xx is a failed probe, never a new destination.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "connect")?;
    let resp = client.get(format!("http://127.0.0.1:{port}{path}")).send().await.map_err(|_| "connect")?;
    if !resp.status().is_success() {
        return Err("protocol");
    }
    resp.json().await.map_err(|_| "protocol")
}

/// GET `path` from the mapped loopback port under the shared discovery policy.
/// Errors are fixed categories (`connect`, `timeout`, `protocol`), never library text.
pub(crate) async fn discover_json(port: u16, path: &str) -> Result<serde_json::Value, String> {
    match tokio::time::timeout(DISCOVERY_DEADLINE, get_json(port, path)).await {
        Ok(r) => r.map_err(str::to_string),
        Err(_) => Err("timeout".to_string()),
    }
}

/// Keep only the validated `/devtools/browser/<id>` path from the response;
/// scheme, host and port come from the map, never from the browser's answer.
fn pinned_ws_url(port: u16, version: &serde_json::Value) -> Result<String, String> {
    let bad = || "protocol".to_string();
    let raw = version.get("webSocketDebuggerUrl").and_then(|v| v.as_str()).ok_or_else(bad)?;
    let url = reqwest::Url::parse(raw).map_err(|_| bad())?;
    let id = url.path().strip_prefix("/devtools/browser/").unwrap_or("");
    if !matches!(url.scheme(), "ws" | "wss")
        || url.query().is_some()
        || url.fragment().is_some()
        || id.is_empty()
        || id.contains('/')
    {
        return Err(bad());
    }
    Ok(format!("ws://127.0.0.1:{port}/devtools/browser/{id}"))
}

/// The browser-level CDP WebSocket URL, confined to `127.0.0.1:<port>`.
pub async fn browser_ws_url(port: u16) -> Result<String, String> {
    pinned_ws_url(port, &discover_json(port, "/json/version").await?)
}

/// One flag per dir: true when its mapped port answers discovery now. Unmapped
/// dirs are false without a request.
pub async fn browser_flags(claude_dir: &Path, dirs: &[&str]) -> Vec<bool> {
    let map = read_port_map(claude_dir).await;
    futures_util::future::join_all(dirs.iter().map(|dir| {
        let port = port_for(&map, dir);
        async move {
            let Some(port) = port else { return false };
            matches!(tokio::time::timeout(PROBE_DEADLINE, browser_ws_url(port)).await, Ok(Ok(_)))
        }
    }))
    .await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    pub(crate) async fn serve(app: axum::Router) -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(l, app).await });
        port
    }

    /// A loopback HTTP server whose `/json/version` answers 200 with `body`.
    pub(crate) async fn serve_version(body: serde_json::Value) -> u16 {
        serve(axum::Router::new().route(
            "/json/version",
            axum::routing::get(move || {
                let b = body.clone();
                async move { axum::Json(b) }
            }),
        ))
        .await
    }

    fn map_dir(tag: &str, entries: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MAP_FILE), entries).unwrap();
        dir
    }

    #[tokio::test]
    async fn flags_are_true_only_for_a_mapped_live_port() {
        let live_port = serve_version(json!({"webSocketDebuggerUrl": "ws://localhost:1/devtools/browser/fake"})).await;
        let dead_port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
        let dir = map_dir("bflags", &format!(r#"{{"/live":{live_port},"/dead":{dead_port}}}"#));
        assert_eq!(browser_flags(&dir, &["/live/x", "/dead", "/other"]).await, vec![true, false, false]);
    }

    #[tokio::test]
    async fn empty_json_is_not_a_browser() {
        let port = serve_version(json!({})).await;
        let dir = map_dir("bempty", &format!(r#"{{"/e":{port}}}"#));
        assert_eq!(browser_flags(&dir, &["/e"]).await, vec![false]);
        assert!(browser_ws_url(port).await.is_err());
    }

    #[tokio::test]
    async fn discovery_never_follows_redirects() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let trap = serve(axum::Router::new().fallback(move || {
            let h = h.clone();
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"webSocketDebuggerUrl": "ws://127.0.0.1:1/devtools/browser/trap"}))
            }
        }))
        .await;
        for status in [301u16, 302, 307, 308] {
            let loc = format!("http://127.0.0.1:{trap}/json/version");
            let port = serve(axum::Router::new().route(
                "/json/version",
                axum::routing::get(move || {
                    let loc = loc.clone();
                    async move { (axum::http::StatusCode::from_u16(status).unwrap(), [(axum::http::header::LOCATION, loc)]) }
                }),
            ))
            .await;
            let dir = map_dir(&format!("bredir{status}"), &format!(r#"{{"/r":{port}}}"#));
            assert_eq!(browser_flags(&dir, &["/r"]).await, vec![false], "{status}");
            assert!(browser_ws_url(port).await.is_err(), "{status}");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0, "the redirect target received a request");
    }

    #[tokio::test]
    async fn discovery_pins_the_websocket_authority() {
        let port = serve_version(json!({"webSocketDebuggerUrl": "ws://elsewhere.invalid:1/devtools/browser/test"})).await;
        assert_eq!(browser_ws_url(port).await.unwrap(), format!("ws://127.0.0.1:{port}/devtools/browser/test"));
        let ws = |u: &str| json!({ "webSocketDebuggerUrl": u });
        for bad in [
            json!({}),
            json!({"webSocketDebuggerUrl": 5}),
            ws("not a url"),
            ws("http://h:1/devtools/browser/a"),
            ws("ws://h:1/other"),
            ws("ws://h:1/devtools/page/a"),
            ws("ws://h:1/devtools/browser/"),
            ws("ws://h:1/devtools/browser/a/b"),
            ws("ws://h:1/devtools/browser/a?x=1"),
            ws("ws://h:1/devtools/browser/a#f"),
            ws("ws://h:1/devtools/browser/../x"),
        ] {
            assert_eq!(pinned_ws_url(port, &bad), Err("protocol".to_string()), "{bad}");
        }
        assert_eq!(
            pinned_ws_url(port, &ws("wss://h:1/devtools/browser/a-1")),
            Ok(format!("ws://127.0.0.1:{port}/devtools/browser/a-1")),
            "scheme and authority come from the map, never the response"
        );
    }

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

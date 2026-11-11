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

/// WSL mounts Windows drives at `/mnt/<letter>` case-insensitively, and a
/// session keeps whatever case its folder was typed in. Paths there compare
/// lowercased; every other path compares exactly.
fn fold(p: &str) -> std::borrow::Cow<'_, str> {
    let b = p.as_bytes();
    let drive = p.starts_with("/mnt/") && b.get(5).is_some_and(u8::is_ascii_alphabetic) && matches!(b.get(6), None | Some(b'/'));
    if drive { p.to_lowercase().into() } else { p.into() }
}

/// Longest key that equals `dir` or is a parent of it, by path component.
pub fn port_for(map: &[(String, u16)], dir: &str) -> Option<u16> {
    let dir = fold(dir);
    map.iter()
        .filter(|(k, _)| {
            let k = fold(k);
            k == "/" || dir == k || dir.starts_with(&format!("{k}/"))
        })
        .max_by_key(|(k, _)| k.len())
        .map(|(_, p)| *p)
}

/// Built once: constructing a client loads the TLS roots (about 25 ms), which
/// would otherwise be paid for every probe of every mapped session on every
/// poll. No proxy and no redirects: a 3xx is a failed probe, never a new
/// destination. No idle pool: a connection must not outlive the runtime that
/// opened it, and a loopback connect is cheap.
fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: std::sync::OnceLock<Option<reqwest::Client>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .pool_max_idle_per_host(0)
                .build()
                .ok()
        })
        .as_ref()
}

async fn get_json(port: u16, path: &str) -> Result<serde_json::Value, &'static str> {
    let client = client().ok_or("connect")?;
    let resp = client.get(format!("http://127.0.0.1:{port}{path}")).send().await.map_err(|_| "connect")?;
    if !resp.status().is_success() {
        return Err("protocol");
    }
    resp.json().await.map_err(|_| "protocol")
}

/// GET `path` from the mapped loopback port under the shared discovery policy.
/// Errors are fixed categories (`connect`, `timeout`, `protocol`), never library text.
pub(crate) async fn discover_json(port: u16, path: &str) -> Result<serde_json::Value, &'static str> {
    tokio::time::timeout(DISCOVERY_DEADLINE, get_json(port, path)).await.unwrap_or(Err("timeout"))
}

/// Keep only the validated `/devtools/browser/<id>` path from the response;
/// scheme, host and port come from the map, never from the browser's answer.
fn pinned_ws_url(port: u16, version: &serde_json::Value) -> Result<String, &'static str> {
    let raw = version["webSocketDebuggerUrl"].as_str().ok_or("protocol")?;
    let url = reqwest::Url::parse(raw).map_err(|_| "protocol")?;
    let id = url.path().strip_prefix("/devtools/browser/").unwrap_or("");
    if !matches!(url.scheme(), "ws" | "wss")
        || url.query().is_some()
        || url.fragment().is_some()
        || id.is_empty()
        || id.contains('/')
    {
        return Err("protocol");
    }
    Ok(format!("ws://127.0.0.1:{port}/devtools/browser/{id}"))
}

/// The browser-level CDP WebSocket URL, confined to `127.0.0.1:<port>`.
pub async fn browser_ws_url(port: u16) -> Result<String, &'static str> {
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

/// Cap on one `text` message, in bytes.
const MAX_TEXT: usize = 4096;

/// What the phone may ask for. Anything else is dropped; the phone never
/// supplies a CDP method or a raw key name outside `KEYS`.
#[derive(Debug, PartialEq)]
pub enum PhoneMsg {
    Tap(f64, f64),
    Scroll { x: f64, y: f64, dx: f64, dy: f64 },
    Text(String),
    Key(&'static str),
    Tab(String),
    Back,
    Reload,
}

/// Allowed named keys and their Windows virtual key codes.
const KEYS: [(&str, u32); 8] = [
    ("Enter", 13),
    ("Tab", 9),
    ("Backspace", 8),
    ("Escape", 27),
    ("ArrowLeft", 37),
    ("ArrowUp", 38),
    ("ArrowRight", 39),
    ("ArrowDown", 40),
];

fn finite_nums<const N: usize>(v: &serde_json::Value) -> Option<[f64; N]> {
    let a = v.as_array().filter(|a| a.len() == N)?;
    let mut out = [0.0; N];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_f64().filter(|f| f.is_finite())?;
    }
    Some(out)
}

/// One JSON object with exactly one known key and a well-formed value.
pub fn parse_phone(text: &str) -> Option<PhoneMsg> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = v.as_object().filter(|o| o.len() == 1)?;
    let (key, val) = obj.iter().next()?;
    match key.as_str() {
        "tap" => {
            let [x, y] = finite_nums(val)?;
            Some(PhoneMsg::Tap(x, y))
        }
        "scroll" => {
            let [x, y, dx, dy] = finite_nums(val)?;
            Some(PhoneMsg::Scroll { x, y, dx, dy })
        }
        "text" => val.as_str().filter(|t| !t.is_empty() && t.len() <= MAX_TEXT).map(|t| PhoneMsg::Text(t.to_string())),
        "key" => KEYS.iter().find(|(n, _)| Some(*n) == val.as_str()).map(|(n, _)| PhoneMsg::Key(n)),
        "tab" => val.as_str().filter(|t| !t.is_empty()).map(|t| PhoneMsg::Tab(t.to_string())),
        "nav" => match val.as_str()? {
            "back" => Some(PhoneMsg::Back),
            "reload" => Some(PhoneMsg::Reload),
            _ => None,
        },
        _ => None,
    }
}

/// `Input.dispatchKeyEvent` params for a named key, in send order. Enter also
/// sends a `char` event. Every key carries its virtual key code: without one a
/// page can ignore it (a Backspace then deletes nothing).
pub fn key_events(key: &str) -> Option<Vec<serde_json::Value>> {
    let (name, vk) = KEYS.iter().find(|(n, _)| *n == key)?;
    let ev = |kind: &str| serde_json::json!({"type": kind, "key": name, "code": name, "windowsVirtualKeyCode": vk});
    let mut out = vec![ev("rawKeyDown")];
    if *name == "Enter" {
        let mut c = ev("char");
        c["text"] = "\r".into();
        out.push(c);
    }
    out.push(ev("keyUp"));
    Some(out)
}

/// One key press per Unicode scalar. Only ASCII letters and digits carry a
/// virtual key code (pages that filter by `keyCode` drop them otherwise);
/// space and everything else deliberately has none.
pub fn char_events(c: char) -> [serde_json::Value; 2] {
    let s = c.to_string();
    let mut down = serde_json::json!({"type": "keyDown", "key": s, "text": s});
    let mut up = serde_json::json!({"type": "keyUp", "key": s});
    if c.is_ascii_alphanumeric() {
        let vk = c.to_ascii_uppercase() as u32;
        down["windowsVirtualKeyCode"] = vk.into();
        up["windowsVirtualKeyCode"] = vk.into();
    }
    [down, up]
}

/// Size of the page, in CSS pixels, that the displayed frame belongs to.
pub struct Viewport {
    pub width: f64,
    pub height: f64,
}

impl Viewport {
    /// `None` unless both dimensions are positive and finite.
    pub fn new(width: f64, height: f64) -> Option<Viewport> {
        (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0).then_some(Viewport { width, height })
    }
}

pub fn clamp_point(x: f64, y: f64, vp: &Viewport) -> (f64, f64) {
    (x.clamp(0.0, vp.width), y.clamp(0.0, vp.height))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::collect::term::tests::tempdir;
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
        let dir = tempdir(tag);
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
            assert_eq!(pinned_ws_url(port, &bad), Err("protocol"), "{bad}");
        }
        assert_eq!(
            pinned_ws_url(port, &ws("wss://h:1/devtools/browser/a-1")),
            Ok(format!("ws://127.0.0.1:{port}/devtools/browser/a-1")),
            "scheme and authority come from the map, never the response"
        );
    }

    #[test]
    fn phone_messages_are_validated() {
        assert_eq!(parse_phone(r#"{"tap":[10.5,20]}"#), Some(PhoneMsg::Tap(10.5, 20.0)));
        assert_eq!(parse_phone(r#"{"scroll":[1,2,3,-4]}"#), Some(PhoneMsg::Scroll { x: 1.0, y: 2.0, dx: 3.0, dy: -4.0 }));
        assert_eq!(parse_phone(r#"{"text":"é"}"#), Some(PhoneMsg::Text("é".into())));
        assert_eq!(parse_phone(r#"{"key":"Backspace"}"#), Some(PhoneMsg::Key("Backspace")));
        assert_eq!(parse_phone(r#"{"tab":"P1"}"#), Some(PhoneMsg::Tab("P1".into())));
        assert_eq!(parse_phone(r#"{"nav":"back"}"#), Some(PhoneMsg::Back));
        assert_eq!(parse_phone(r#"{"nav":"reload"}"#), Some(PhoneMsg::Reload));
        for bad in [r#"{"tap":[1]}"#, r#"{"tap":["a",1]}"#, r#"{"scroll":[1,2,3]}"#, r#"{"key":"F5"}"#, r#"{"nav":"forward"}"#,
                    r#"{"text":""}"#, r#"{"tab":""}"#, r#"{"eval":"x"}"#, r#"{"tap":[1,2],"text":"x"}"#, "[]", "nope"] {
            assert_eq!(parse_phone(bad), None, "{bad}");
        }
        let long = format!(r#"{{"text":"{}"}}"#, "a".repeat(MAX_TEXT + 1));
        assert_eq!(parse_phone(&long), None);
        let at_cap = format!(r#"{{"text":"{}"}}"#, "a".repeat(MAX_TEXT));
        assert!(parse_phone(&at_cap).is_some(), "exactly MAX_TEXT bytes is allowed");
    }

    #[test]
    fn keys_carry_virtual_key_codes() {
        let enter = key_events("Enter").unwrap();
        assert_eq!(enter.len(), 3);
        assert_eq!(enter[0]["type"], "rawKeyDown");
        assert_eq!(enter[0]["windowsVirtualKeyCode"], 13);
        assert_eq!(enter[1]["type"], "char");
        assert_eq!(enter[1]["text"], "\r");
        assert_eq!(enter[2]["type"], "keyUp");
        for (k, vk) in [("Tab", 9), ("Backspace", 8), ("Escape", 27), ("ArrowLeft", 37), ("ArrowUp", 38), ("ArrowRight", 39), ("ArrowDown", 40)] {
            let ev = key_events(k).unwrap();
            assert_eq!(ev.len(), 2, "{k}");
            assert_eq!(ev[0]["type"], "rawKeyDown");
            assert_eq!(ev[0]["windowsVirtualKeyCode"], vk, "{k}");
            assert_eq!(ev[0]["key"], k);
            assert_eq!(ev[1]["type"], "keyUp");
        }
        assert!(key_events("F5").is_none());
    }

    #[test]
    fn chars_become_one_key_press_each() {
        let [down, up] = char_events('a');
        assert_eq!((down["type"].as_str(), down["text"].as_str(), down["windowsVirtualKeyCode"].as_u64()), (Some("keyDown"), Some("a"), Some(65)));
        assert_eq!((up["type"].as_str(), up["windowsVirtualKeyCode"].as_u64()), (Some("keyUp"), Some(65)));
        assert_eq!(char_events('7')[0]["windowsVirtualKeyCode"], 55);
        for c in ['$', 'é', '😀', ' '] {
            let [down, _] = char_events(c);
            assert_eq!(down["text"], c.to_string());
            assert!(down.get("windowsVirtualKeyCode").is_none(), "{c:?} has no VK");
        }
    }

    #[test]
    fn clamp_keeps_points_inside_the_viewport() {
        let vp = Viewport { width: 1000.0, height: 600.0 };
        assert_eq!(clamp_point(-5.0, 700.0, &vp), (0.0, 600.0));
        assert_eq!(clamp_point(10.0, 20.0, &vp), (10.0, 20.0));
    }

    #[test]
    fn viewport_requires_positive_finite_dimensions() {
        assert!(Viewport::new(1000.0, 600.0).is_some());
        for (w, h) in [(0.0, 600.0), (1000.0, 0.0), (-1.0, 600.0), (f64::NAN, 600.0), (1000.0, f64::INFINITY)] {
            assert!(Viewport::new(w, h).is_none(), "{w} x {h}");
        }
    }

    #[test]
    fn prefix_match_is_by_path_component() {
        let map = vec![("/mnt/d/p/Downloads/JOBS".to_string(), 9301), ("/mnt/d/p".to_string(), 9400)];
        assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS"), Some(9301));
        assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS/sub"), Some(9301));
        assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS2"), Some(9400), "sibling falls to the parent key");
        assert_eq!(port_for(&map, "/mnt/d/q"), None);
    }

    #[test]
    fn windows_drive_paths_match_in_any_case() {
        // WSL mounts Windows drives case-insensitively: a session launched as
        // /mnt/d/p/downloads/jobs runs in the JOBS folder.
        let map = vec![("/mnt/d/p/Downloads/JOBS".to_string(), 9301), ("/home/p/Proj".to_string(), 9400)];
        assert_eq!(port_for(&map, "/mnt/d/p/downloads/jobs"), Some(9301));
        assert_eq!(port_for(&map, "/mnt/d/p/downloads/jobs/sub"), Some(9301));
        assert_eq!(port_for(&map, "/mnt/d/p/downloads/jobs2"), None, "still by path component");
        assert_eq!(port_for(&map, "/home/p/proj"), None, "Linux paths stay case-sensitive");
    }

    #[tokio::test]
    async fn map_file_rules() {
        let dir = tempdir("bmap");
        assert!(read_port_map(&dir).await.is_empty(), "missing file");
        std::fs::write(dir.join(MAP_FILE), "{not json").unwrap();
        assert!(read_port_map(&dir).await.is_empty(), "malformed file");
        std::fs::write(dir.join(MAP_FILE), r#"{"/a/":9301,"rel":9302,"/b":80,"/c":70000,"/d":"9303","/e":9304}"#).unwrap();
        let mut m = read_port_map(&dir).await;
        m.sort();
        assert_eq!(m, vec![("/a".to_string(), 9301), ("/e".to_string(), 9304)]);
    }
}

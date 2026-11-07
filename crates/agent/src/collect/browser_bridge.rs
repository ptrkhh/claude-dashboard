//! `GET /api/browser`: stream one tab of a session's Chromium browser to the
//! phone over a WebSocket. The phone gets JPEG frames and a tab list; this side
//! speaks a narrow set of CDP methods and never relays raw CDP either way.
//! Nothing the browser or the phone sends may reach the log or a close reason:
//! only the fixed categories below.

use super::browser::{browser_ws_url, Viewport};
use super::cdp::{page_order, Cdp, Event};
use super::ctx::Ctx;
use super::term::{send_or_drop, PING_EVERY};
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use base64::Engine;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// Bound on the best-effort stop/detach when a view ends.
const CLEANUP: Duration = Duration::from_secs(2);

/// One page target. `origin` is a tuple origin or JSON null, never a URL.
struct Tab {
    id: String,
    title: String,
    origin: Value,
}

/// The tab being streamed: its target id and the CDP session attached to it.
struct View {
    target: String,
    session: String,
}

enum Exit {
    /// The phone left; nothing to tell it.
    Phone,
    /// Discovery or connect failed before any view started: 4002.
    NotRunning(&'static str),
    /// The browser's socket closed: 4003.
    Gone,
    /// A required CDP method is missing: 4004.
    Unsupported,
    /// Anything else, as a fixed category: 1011.
    Failed(&'static str),
}

/// Map a `Cdp` error to one of the fixed categories. Nothing else is ever
/// logged, whatever the string holds.
fn category(e: &str) -> &'static str {
    match e {
        "connect" => "connect",
        "timeout" => "timeout",
        "unsupported" => "unsupported",
        _ => "protocol",
    }
}

fn failed(e: &str) -> Exit {
    match category(e) {
        "unsupported" => Exit::Unsupported,
        // The browser's socket went away mid-session.
        "connect" => Exit::Gone,
        c => Exit::Failed(c),
    }
}

/// The page's tuple origin (`scheme://host[:port]`), or JSON null for an
/// opaque or unparseable URL. Never falls back to the URL itself, which can
/// carry file paths, data or tokens.
fn origin_of(url: &str) -> Value {
    match reqwest::Url::parse(url) {
        Ok(u) if u.origin().is_tuple() => Value::String(u.origin().ascii_serialization()),
        _ => Value::Null,
    }
}

fn tab_of(info: &Value) -> Option<Tab> {
    if info["type"] != "page" {
        return None;
    }
    Some(Tab {
        id: info["targetId"].as_str()?.to_string(),
        title: info["title"].as_str().unwrap_or("").to_string(),
        origin: origin_of(info["url"].as_str().unwrap_or("")),
    })
}

fn upsert(tabs: &mut Vec<Tab>, tab: Tab) {
    match tabs.iter_mut().find(|t| t.id == tab.id) {
        Some(t) => *t = tab,
        None => tabs.push(tab),
    }
}

fn tabs_msg(tabs: &[Tab], view: Option<&View>) -> Message {
    let list: Vec<Value> = tabs.iter().map(|t| json!({"id": t.id, "title": t.title, "origin": t.origin})).collect();
    Message::Text(json!({"tabs": list, "active": view.map(|v| v.target.as_str())}).to_string().into())
}

/// The first tab `/json/list` names that `getTargets` also lists, else the
/// first listed page. `/json/list` order is a hint, not activity order.
fn initial_tab<'a>(tabs: &'a [Tab], order: &[String]) -> Option<&'a Tab> {
    order.iter().find_map(|id| tabs.iter().find(|t| &t.id == id)).or_else(|| tabs.first())
}

async fn start_view(cdp: &Cdp, target: &str) -> Result<View, String> {
    let r = cdp.call("Target.attachToTarget", json!({"targetId": target, "flatten": true}), None).await?;
    let session = r["sessionId"].as_str().ok_or("protocol")?.to_string();
    cdp.call("Page.enable", json!({}), Some(&session)).await?;
    cdp.call("Page.startScreencast", json!({"format": "jpeg", "quality": 60, "maxWidth": 1280}), Some(&session)).await?;
    Ok(View { target: target.to_string(), session })
}

/// Best effort and bounded: the browser, its tab and Playwright stay untouched.
async fn stop_view(cdp: &Cdp, view: &View) {
    let _ = tokio::time::timeout(CLEANUP, async {
        let _ = cdp.call("Page.stopScreencast", json!({}), Some(&view.session)).await;
        let _ = cdp.call("Target.detachFromTarget", json!({"sessionId": view.session}), None).await;
    })
    .await;
}

/// Metadata for the phone, copied from CDP; `None` unless the page size is
/// positive and finite and the other two fields are finite.
fn frame_meta(m: &Value) -> Option<Value> {
    let n = |k: &str| m[k].as_f64().filter(|f| f.is_finite());
    let (w, h) = (n("deviceWidth")?, n("deviceHeight")?);
    Viewport::new(w, h)?;
    Some(json!({"frame": {
        "deviceWidth": w, "deviceHeight": h, "offsetTop": n("offsetTop")?, "pageScaleFactor": n("pageScaleFactor")?}}))
}

/// Metadata text, then the JPEG, then the ack, so a slow link lowers the frame
/// rate instead of queueing frames. A frame that cannot be shown is still
/// acked or the browser stops sending.
async fn relay_frame(cdp: &Cdp, ws: &mut WebSocket, e: &Event) -> Option<Exit> {
    let p = &e.params;
    let jpeg = p["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok());
    if let (Some(meta), Some(jpeg)) = (frame_meta(&p["metadata"]), jpeg) {
        if !send_or_drop(ws, Message::Text(meta.to_string().into())).await
            || !send_or_drop(ws, Message::Binary(jpeg.into())).await
        {
            return Some(Exit::Failed("send"));
        }
    }
    let ack = p["sessionId"].as_u64()?;
    cdp.call("Page.screencastFrameAck", json!({"sessionId": ack}), e.session.as_deref()).await.err().map(|e| failed(&e))
}

async fn on_event(cdp: &Cdp, ws: &mut WebSocket, tabs: &mut Vec<Tab>, view: &Option<View>, e: Event) -> Option<Exit> {
    match e.method.as_str() {
        "Page.screencastFrame" => {
            // A frame from a session that is no longer the viewed one is dropped.
            if view.as_ref().map(|v| v.session.as_str()) != e.session.as_deref() {
                return None;
            }
            relay_frame(cdp, ws, &e).await
        }
        "Target.targetCreated" | "Target.targetInfoChanged" => {
            upsert(tabs, tab_of(&e.params["targetInfo"])?);
            (!send_or_drop(ws, tabs_msg(tabs, view.as_ref())).await).then_some(Exit::Failed("send"))
        }
        _ => None,
    }
}

async fn pump(
    cdp: &Cdp,
    events: &mut UnboundedReceiver<Event>,
    ws: &mut WebSocket,
    tabs: &mut Vec<Tab>,
    view: &mut Option<View>,
) -> Exit {
    // The list goes first; frames queued while attaching follow it.
    if !send_or_drop(ws, tabs_msg(tabs, view.as_ref())).await {
        return Exit::Failed("send");
    }
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heard = true;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if !std::mem::take(&mut heard) || !send_or_drop(ws, Message::Ping(axum::body::Bytes::new())).await {
                    return Exit::Phone;
                }
            }
            ev = events.recv() => match ev {
                None => return Exit::Gone,
                Some(e) => if let Some(exit) = on_event(cdp, ws, tabs, view, e).await { return exit },
            },
            msg = ws.recv() => {
                heard = true;
                if matches!(msg, Some(Ok(Message::Close(_))) | Some(Err(_)) | None) {
                    return Exit::Phone;
                }
            }
        }
    }
}

async fn run(port: u16, ws: &mut WebSocket) -> Exit {
    let url = match browser_ws_url(port).await {
        Ok(u) => u,
        Err(e) => return Exit::NotRunning(category(&e)),
    };
    let (cdp, mut events) = match Cdp::connect(&url).await {
        Ok(c) => c,
        Err(e) => return Exit::NotRunning(category(&e)),
    };
    // Discover first, so a tab change cannot fall between the snapshot and the
    // subscription; early events queue and are applied after the list is sent.
    if let Err(e) = cdp.call("Target.setDiscoverTargets", json!({"discover": true}), None).await {
        return failed(&e);
    }
    let listed = match cdp.call("Target.getTargets", json!({}), None).await {
        Ok(v) => v,
        Err(e) => return failed(&e),
    };
    let mut tabs: Vec<Tab> = listed["targetInfos"].as_array().map(|a| a.iter().filter_map(tab_of).collect()).unwrap_or_default();
    let mut view = None;
    if let Some(t) = initial_tab(&tabs, &page_order(port).await) {
        match start_view(&cdp, &t.id).await {
            Ok(v) => view = Some(v),
            Err(e) => return failed(&e),
        }
    }
    let exit = pump(&cdp, &mut events, ws, &mut tabs, &mut view).await;
    if let Some(v) = &view {
        stop_view(&cdp, v).await;
    }
    exit // `cdp` drops here, closing the socket and ending its tasks
}

/// Run one browser connection to the end, then tell the phone why it ended.
pub async fn bridge(ctx: Arc<Ctx>, name: String, port: u16, mut ws: WebSocket) {
    ctx.host.log.push(format!("browser open {name}"));
    let close = match run(port, &mut ws).await {
        Exit::Phone => None,
        Exit::Gone => Some((4003, "Browser closed")),
        Exit::NotRunning(c) => {
            ctx.host.log.push(format!("browser failed {name}: {c}"));
            Some((4002, "Browser not running"))
        }
        Exit::Unsupported => {
            ctx.host.log.push(format!("browser failed {name}: unsupported"));
            Some((4004, "Browser view unsupported"))
        }
        Exit::Failed(c) => {
            ctx.host.log.push(format!("browser failed {name}: {c}"));
            Some((1011, "Browser connection failed"))
        }
    };
    if let Some((code, reason)) = close {
        let _ = ws.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
        // Wait for the phone's Close reply; dropping the socket with a frame
        // unread can turn the close code into a plain 1006.
        let drain = async { while let Some(Ok(_)) = ws.recv().await {} };
        let _ = tokio::time::timeout(Duration::from_secs(1), drain).await;
    }
    ctx.host.log.push(format!("browser close {name}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::browser::MAP_FILE;
    use crate::collect::cdp::fake::{fake_edge, FakeEdge};
    use crate::collect::term::tests::{have_tmux, status_of, tempdir, Session, Ws};
    use crate::http::serve::{serve, tests::cfg_for, Bound};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message as WsMsg};

    const WAIT: Duration = Duration::from_secs(5);
    const CANARIES: [&str; 4] = ["URL_CANARY", "TITLE_CANARY", "typed-text", "CANARY"];
    /// The only CDP methods cdash-agent may call.
    const ALLOWED: [&str; 13] = [
        "Target.getTargets", "Target.setDiscoverTargets", "Target.attachToTarget", "Target.detachFromTarget",
        "Page.enable", "Page.startScreencast", "Page.stopScreencast", "Page.screencastFrameAck",
        "Page.getNavigationHistory", "Page.navigateToHistoryEntry", "Page.reload",
        "Input.dispatchMouseEvent", "Input.dispatchKeyEvent",
    ];

    struct Rig {
        b: Bound,
        edge: FakeEdge,
        name: String,
        claude: PathBuf,
        work: PathBuf,
        _s: Session,
    }

    /// A tmux session in `work`, a fake browser, and an agent whose map sends
    /// `work` to that browser. `None` when tmux is missing (the test skips).
    async fn rig(tag: &str, pages: &[(&str, &str, &str)]) -> Option<Rig> {
        if !have_tmux() {
            return None;
        }
        let claude = tempdir(&format!("bb-{tag}"));
        let work = std::fs::canonicalize(tempdir(&format!("bbw-{tag}"))).unwrap();
        let s = Session::in_dir(&format!("bb-{tag}"), &work);
        let edge = fake_edge(pages).await;
        let b = serve(cfg_for(claude.clone())).await.unwrap();
        let rig = Rig { name: s.0.clone(), b, edge, claude, work, _s: s };
        rig.remap(Some(rig.edge.port));
        Some(rig)
    }

    impl Rig {
        /// Point the session's folder at `port`, or at nothing.
        fn remap(&self, port: Option<u16>) {
            let map = port.map(|p| json!({ self.work.to_string_lossy(): p })).unwrap_or_else(|| json!({}));
            std::fs::write(self.claude.join(MAP_FILE), map.to_string()).unwrap();
        }
        async fn open(&self) -> Ws {
            connect(self.b.addr, &format!("name={}", self.name), None).await.unwrap()
        }
        /// Log messages without their `HH:MM:SS ` stamp.
        fn log(&self) -> Vec<String> {
            self.b.ctx.host.log.lines().into_iter().map(|l| l.split_once(' ').map_or(l.clone(), |(_, m)| m.to_string())).collect()
        }
        fn assert_no_canary(&self) {
            for line in self.log() {
                for c in CANARIES {
                    assert!(!line.contains(c), "log line leaks {c}: {line}");
                }
            }
        }
    }

    async fn connect(addr: std::net::SocketAddr, query: &str, origin: Option<&str>) -> Result<Ws, tungstenite::Error> {
        let mut req = format!("ws://{addr}/api/browser?{query}").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
    }

    async fn next_msg(ws: &mut Ws) -> WsMsg {
        tokio::time::timeout(WAIT, async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMsg::Ping(_) | WsMsg::Pong(_))) => {}
                    Some(Ok(m)) => return m,
                    other => panic!("socket ended: {other:?}"),
                }
            }
        })
        .await
        .expect("no message within 5s")
    }

    async fn text(ws: &mut Ws) -> Value {
        match next_msg(ws).await {
            WsMsg::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
            m => panic!("expected a text frame, got {m:?}"),
        }
    }

    async fn close_of(ws: &mut Ws) -> (u16, String) {
        loop {
            match next_msg(ws).await {
                WsMsg::Close(Some(f)) => return (u16::from(f.code), f.reason.to_string()),
                WsMsg::Close(None) => panic!("closed without a code"),
                _ => {}
            }
        }
    }

    async fn call_until(edge: &mut FakeEdge, method: &str) -> (Value, Option<String>) {
        tokio::time::timeout(WAIT, async {
            loop {
                let (m, p, s) = edge.calls.recv().await.expect("fake browser gone");
                if m == method {
                    return (p, s);
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{method} was never called"))
    }

    /// Everything the bridge calls until it has been quiet for `settle`.
    async fn drain_calls(edge: &mut FakeEdge, settle: Duration) -> Vec<(String, Value, Option<String>)> {
        let mut out = Vec::new();
        while let Ok(Some(c)) = tokio::time::timeout(settle, edge.calls.recv()).await {
            out.push(c);
        }
        out
    }

    async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..50 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("never happened: {what}");
    }

    fn frame_event(session: &str, data: &str, ack_id: u64, w: f64, h: f64) -> Value {
        json!({"method": "Page.screencastFrame", "sessionId": session, "params": {
            "data": data, "sessionId": ack_id,
            "metadata": {"deviceWidth": w, "deviceHeight": h, "offsetTop": 0, "pageScaleFactor": 1}}})
    }

    fn page_event(method: &str, id: &str, url: &str) -> Value {
        json!({"method": method, "params": {"targetInfo": {"targetId": id, "type": "page", "url": url, "title": "T"}}})
    }

    #[test]
    fn origins_are_tuples_or_null() {
        assert_eq!(origin_of("https://user:pw@b.example:8443/p?q=URL_CANARY#f"), json!("https://b.example:8443"));
        assert_eq!(origin_of("http://a.example/"), json!("http://a.example"));
        for opaque in ["data:text/plain,URL_CANARY", "file:///home/u/secret.html", "about:blank", "javascript:alert(1)", "not a url", ""] {
            assert_eq!(origin_of(opaque), Value::Null, "{opaque:?}");
        }
    }

    #[tokio::test]
    async fn refusals_happen_before_upgrade() {
        let Some(r) = rig("refuse", &[]).await else { return };
        let q = format!("name={}", r.name);
        assert_eq!(status_of(connect(r.b.addr, &q, Some("https://evil.example")).await), 403);
        assert_eq!(status_of(connect(r.b.addr, "name=other", None).await), 400);
        assert_eq!(status_of(connect(r.b.addr, "name=cdash-missing", None).await), 404);
        r.remap(None);
        assert_eq!(status_of(connect(r.b.addr, &q, None).await), 404, "a session in an unmapped folder");
        let log = r.log();
        for line in ["browser refused: origin", "browser refused: name", "browser refused: session", "browser refused: mapping"] {
            assert!(log.iter().any(|l| l == line), "missing {line:?} in {log:?}");
        }
    }

    #[tokio::test]
    async fn frame_is_relayed_then_acked() {
        let Some(mut r) = rig("frame", &[("P1", "https://a.example/login?x=1", "A")]).await else { return };
        let mut ws = r.open().await;
        let tabs = text(&mut ws).await;
        assert_eq!(tabs["active"], "P1");
        assert_eq!(tabs["tabs"], json!([{"id": "P1", "title": "A", "origin": "https://a.example"}]));
        let (p, s) = call_until(&mut r.edge, "Page.startScreencast").await;
        assert_eq!((p["format"].as_str(), p["quality"].as_u64(), p["maxWidth"].as_u64(), s.as_deref()), (Some("jpeg"), Some(60), Some(1280), Some("S-P1")));

        r.edge.events.send(frame_event("S-P1", "SlBFRw==", 7, 1000.0, 600.0)).unwrap();
        let meta = text(&mut ws).await;
        assert_eq!(meta, json!({"frame": {"deviceWidth": 1000.0, "deviceHeight": 600.0, "offsetTop": 0.0, "pageScaleFactor": 1.0}}));
        assert_eq!(next_msg(&mut ws).await, WsMsg::binary(b"JPEG".to_vec()));
        let (p, s) = call_until(&mut r.edge, "Page.screencastFrameAck").await;
        assert_eq!((p["sessionId"].as_u64(), s.as_deref()), (Some(7), Some("S-P1")));
    }

    #[tokio::test]
    async fn a_frame_with_bad_metadata_is_acked_but_never_relayed() {
        let Some(mut r) = rig("badframe", &[("P1", "https://a.example/", "A")]).await else { return };
        let mut ws = r.open().await;
        text(&mut ws).await;
        call_until(&mut r.edge, "Page.startScreencast").await;
        for (i, (w, h)) in [(0.0, 600.0), (1000.0, -1.0)].into_iter().enumerate() {
            r.edge.events.send(frame_event("S-P1", "QkFE", 100 + i as u64, w, h)).unwrap();
        }
        r.edge.events.send(frame_event("S-P1", "!!not base64!!", 102, 1000.0, 600.0)).unwrap();
        r.edge.events.send(frame_event("S-OLD", "T0xE", 103, 1000.0, 600.0)).unwrap();
        r.edge.events.send(frame_event("S-P1", "R09PRA==", 104, 1000.0, 600.0)).unwrap();
        assert_eq!(text(&mut ws).await["frame"]["deviceWidth"], 1000.0);
        assert_eq!(next_msg(&mut ws).await, WsMsg::binary(b"GOOD".to_vec()), "only the valid frame reaches the phone");
        let acks: Vec<u64> = drain_calls(&mut r.edge, Duration::from_millis(300)).await.into_iter()
            .filter(|(m, ..)| m == "Page.screencastFrameAck").filter_map(|(_, p, _)| p["sessionId"].as_u64()).collect();
        assert_eq!(acks, vec![100, 101, 102, 104], "bad frames are acked so the stream keeps flowing; a stale session's frame is ignored");
    }

    #[tokio::test]
    async fn browser_not_running_closes_4002() {
        let Some(r) = rig("down", &[]).await else { return };
        let dead = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
        r.remap(Some(dead));
        let mut ws = r.open().await;
        assert_eq!(close_of(&mut ws).await, (4002, "Browser not running".to_string()));
    }

    #[tokio::test]
    async fn browser_going_away_closes_4003() {
        let Some(r) = rig("gone", &[("P1", "https://a.example/", "A")]).await else { return };
        let mut ws = r.open().await;
        text(&mut ws).await;
        r.edge.drop_socket.notify_one();
        assert_eq!(close_of(&mut ws).await, (4003, "Browser closed".to_string()));
    }

    #[tokio::test]
    async fn log_never_contains_urls() {
        let Some(r) = rig("log", &[("P1", "https://a.example/secret?token=URL_CANARY", "TITLE_CANARY")]).await else { return };
        let mut ws = r.open().await;
        text(&mut ws).await;
        ws.send(WsMsg::Close(None)).await.unwrap();
        let open = format!("browser open {}", r.name);
        let close = format!("browser close {}", r.name);
        eventually("close is logged", || r.log().contains(&close)).await;
        let log = r.log();
        assert!(log.contains(&open), "{log:?}");
        assert!(log.iter().all(|l| !l.contains("a.example")), "{log:?}");
        r.assert_no_canary();
    }

    #[tokio::test]
    async fn errors_never_expose_canaries() {
        let canary = "https://secret.example/?token=URL_CANARY TITLE_CANARY typed-text";
        let err = |code: i64| Some(json!({"error": {"code": code, "message": canary}}));

        // Discovery fails with a hostile body.
        let Some(r) = rig("canary-disc", &[]).await else { return };
        let hostile = crate::collect::browser::tests::serve(axum::Router::new().fallback(move || async move {
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, canary)
        })).await;
        r.remap(Some(hostile));
        assert_eq!(close_of(&mut r.open().await).await, (4002, "Browser not running".to_string()));
        r.assert_no_canary();

        // A CDP call fails while streaming starts.
        let Some(r) = rig("canary-cdp", &[("P1", "https://a.example/", "A")]).await else { return };
        r.edge.replies.lock().unwrap().insert("Page.startScreencast".into(), err(-32000));
        assert_eq!(close_of(&mut r.open().await).await, (1011, "Browser connection failed".to_string()));
        eventually("failure is logged", || r.log().contains(&format!("browser failed {}: protocol", r.name))).await;
        r.assert_no_canary();

        // Rejected Origin and name never reach the log.
        let Some(r) = rig("canary-in", &[]).await else { return };
        assert_eq!(status_of(connect(r.b.addr, &format!("name={}", r.name), Some("https://CANARY.example")).await), 403);
        assert_eq!(status_of(connect(r.b.addr, "name=other-URL_CANARY", None).await), 400);
        r.assert_no_canary();

        // Cleanup calls fail when the phone leaves.
        let Some(r) = rig("canary-clean", &[("P1", "https://a.example/", "A")]).await else { return };
        {
            let mut replies = r.edge.replies.lock().unwrap();
            replies.insert("Page.stopScreencast".into(), err(-32000));
            replies.insert("Target.detachFromTarget".into(), err(-32000));
        }
        let mut ws = r.open().await;
        text(&mut ws).await;
        ws.send(WsMsg::Close(None)).await.unwrap();
        eventually("close is logged", || r.log().contains(&format!("browser close {}", r.name))).await;
        r.assert_no_canary();
    }

    #[tokio::test]
    async fn missing_cdp_method_closes_4004() {
        let Some(r) = rig("unsupported", &[("P1", "https://a.example/", "A")]).await else { return };
        r.edge.replies.lock().unwrap().insert(
            "Page.startScreencast".into(),
            Some(json!({"error": {"code": -32601, "message": "URL_CANARY"}})),
        );
        assert_eq!(close_of(&mut r.open().await).await, (4004, "Browser view unsupported".to_string()));
        r.assert_no_canary();
    }

    #[tokio::test]
    async fn opaque_origins_are_null() {
        let pages = [
            ("D", "data:text/plain,URL_CANARY", "d"),
            ("F", "file:///home/u/URL_CANARY.html", "f"),
            ("B", "not a url URL_CANARY", "b"),
            ("H", "https://user:pw@b.example/p/URL_CANARY?q=URL_CANARY#URL_CANARY", "h"),
        ];
        let Some(r) = rig("origins", &pages).await else { return };
        let tabs = text(&mut r.open().await).await;
        let origins: Vec<Value> = tabs["tabs"].as_array().unwrap().iter().map(|t| t["origin"].clone()).collect();
        assert_eq!(origins, vec![Value::Null, Value::Null, Value::Null, json!("https://b.example")]);
        assert!(!tabs.to_string().contains("URL_CANARY"), "{tabs}");
    }

    #[tokio::test]
    async fn phone_close_detaches_without_closing_browser() {
        for (tag, failing_cleanup) in [("detach", false), ("detach-fail", true)] {
            let Some(mut r) = rig(tag, &[("P1", "https://a.example/", "A")]).await else { return };
            if failing_cleanup {
                r.edge.replies.lock().unwrap().insert(
                    "Page.stopScreencast".into(),
                    Some(json!({"error": {"code": -32000, "message": "URL_CANARY"}})),
                );
            }
            let mut ws = r.open().await;
            text(&mut ws).await;
            ws.send(WsMsg::Close(None)).await.unwrap();
            eventually("the CDP socket closes", || r.edge.connections.load(Ordering::SeqCst) == 0).await;
            let calls = drain_calls(&mut r.edge, Duration::from_millis(300)).await;
            let find = |m: &str| calls.iter().find(|(c, ..)| c == m);
            assert_eq!(find("Page.stopScreencast").and_then(|c| c.2.clone()).as_deref(), Some("S-P1"), "{tag}");
            assert_eq!(find("Target.detachFromTarget").and_then(|c| c.1["sessionId"].as_str().map(str::to_string)).as_deref(), Some("S-P1"), "{tag}: detach even when stop failed");
            for (m, ..) in &calls {
                assert!(ALLOWED.contains(&m.as_str()), "{m} is not an allowed CDP method");
            }
        }
    }

    #[tokio::test]
    async fn no_pages_waits_for_user_selection() {
        let Some(mut r) = rig("nopages", &[]).await else { return };
        let mut ws = r.open().await;
        assert_eq!(text(&mut ws).await, json!({"tabs": [], "active": null}));
        r.edge.events.send(page_event("Target.targetCreated", "P9", "https://z.example/")).unwrap();
        assert_eq!(text(&mut ws).await, json!({"tabs": [{"id": "P9", "title": "T", "origin": "https://z.example"}], "active": null}));
        let calls = drain_calls(&mut r.edge, Duration::from_millis(300)).await;
        assert!(calls.iter().all(|(m, ..)| m != "Target.attachToTarget" && m != "Page.startScreencast"), "{calls:?}");
    }
}

//! `GET /api/browser`: stream one tab of a session's Chromium browser to the
//! phone over a WebSocket. The phone gets JPEG frames and a tab list; this side
//! speaks a narrow set of CDP methods and never relays raw CDP either way.
//! Nothing the browser or the phone sends may reach the log or a close reason:
//! only the fixed categories below.

use super::browser::{browser_ws_url, char_events, clamp_point, key_events, parse_phone, PhoneMsg, Viewport};
use super::cdp::{page_order, Cdp, Event};
use super::ctx::Ctx;
use super::term::{close_and_drain, ping_interval, send_or_drop};
use axum::extract::ws::{Message, WebSocket};
use base64::Engine;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// Bound on the best-effort stop/detach when a view ends.
const CLEANUP: Duration = Duration::from_secs(2);

/// One page target. `origin` is a tuple origin or JSON null, never a URL.
#[derive(PartialEq)]
struct Tab {
    id: String,
    title: String,
    origin: Value,
}

/// The tab being streamed: its target id and the CDP session attached to it.
/// `viewport` is the page size of the last frame relayed to the phone; input
/// is dropped until there is one, and it dies with the view.
struct View {
    target: String,
    session: String,
    viewport: Option<Viewport>,
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

/// Errors from `Cdp` and discovery are already fixed categories
/// (`connect`, `timeout`, `protocol`, `unsupported`); nothing else is logged.
fn failed(e: &'static str) -> Exit {
    match e {
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

/// Insert or replace by id; `false` when the list already held exactly this.
fn upsert(tabs: &mut Vec<Tab>, tab: Tab) -> bool {
    match tabs.iter_mut().find(|t| t.id == tab.id) {
        Some(t) if *t == tab => false,
        Some(t) => {
            *t = tab;
            true
        }
        None => {
            tabs.push(tab);
            true
        }
    }
}

fn tabs_msg(tabs: &[Tab], view: Option<&View>) -> Message {
    let list: Vec<Value> = tabs.iter().map(|t| json!({"id": t.id, "title": t.title, "origin": t.origin})).collect();
    Message::Text(json!({"tabs": list, "active": view.map(|v| v.target.as_str())}).to_string().into())
}

/// In `/json/list` order (a hint, not activity order), then `getTargets`
/// order: the first page with a web origin, else the first page. Browsers list
/// their own dialogs and WebUI (`edge://`, `chrome://`) as pages too; headed
/// Edge puts a sync dialog that never paints first. Those stay pickable.
fn initial_tab<'a>(tabs: &'a [Tab], order: &[String]) -> Option<&'a Tab> {
    let ranked = || order.iter().filter_map(|id| tabs.iter().find(|t| &t.id == id)).chain(tabs);
    ranked().find(|t| !t.origin.is_null()).or_else(|| ranked().next())
}

async fn start_view(cdp: &Cdp, target: &str) -> Result<View, &'static str> {
    let r = cdp.call("Target.attachToTarget", json!({"targetId": target, "flatten": true}), None).await?;
    let session = r["sessionId"].as_str().ok_or("protocol")?.to_string();
    cdp.call("Page.enable", json!({}), Some(&session)).await?;
    cdp.call("Page.startScreencast", json!({"format": "jpeg", "quality": 60, "maxWidth": 1280}), Some(&session)).await?;
    Ok(View { target: target.to_string(), session, viewport: None })
}

/// Best effort and bounded: the browser, its tab and Playwright stay untouched.
async fn stop_view(cdp: &Cdp, view: &View) {
    let _ = tokio::time::timeout(CLEANUP, async {
        let _ = cdp.call("Page.stopScreencast", json!({}), Some(&view.session)).await;
        let _ = cdp.call("Target.detachFromTarget", json!({"sessionId": view.session}), None).await;
    })
    .await;
}

/// The frame's page size in CSS pixels, `None` unless positive and finite.
/// It is all the phone needs to map a tap on the picture back onto the page.
fn frame_size(m: &Value) -> Option<Viewport> {
    Viewport::new(m["deviceWidth"].as_f64()?, m["deviceHeight"].as_f64()?)
}

/// Metadata text, then the JPEG, then the ack, so a slow link lowers the frame
/// rate instead of queueing frames. A frame that cannot be shown is still
/// acked or the browser stops sending.
async fn relay_frame(cdp: &Cdp, ws: &mut WebSocket, view: &mut View, e: &Event) -> Option<Exit> {
    let p = &e.params;
    let jpeg = p["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok());
    if let (Some(vp), Some(jpeg)) = (frame_size(&p["metadata"]), jpeg) {
        let meta = json!({"frame": {"deviceWidth": vp.width, "deviceHeight": vp.height}});
        if !send_or_drop(ws, Message::Text(meta.to_string().into())).await
            || !send_or_drop(ws, Message::Binary(jpeg.into())).await
        {
            return Some(Exit::Failed("send"));
        }
        view.viewport = Some(vp);
    }
    let ack = p["sessionId"].as_u64()?;
    cdp.call("Page.screencastFrameAck", json!({"sessionId": ack}), e.session.as_deref()).await.err().map(failed)
}

async fn on_event(cdp: &Cdp, ws: &mut WebSocket, tabs: &mut Vec<Tab>, view: &mut Option<View>, e: Event) -> Option<Exit> {
    match e.method.as_str() {
        "Page.screencastFrame" => {
            // A frame from a session that is no longer the viewed one is dropped.
            let v = view.as_mut().filter(|v| Some(v.session.as_str()) == e.session.as_deref())?;
            relay_frame(cdp, ws, v, &e).await
        }
        "Target.targetCreated" | "Target.targetInfoChanged" => {
            if !upsert(tabs, tab_of(&e.params["targetInfo"])?) {
                return None; // nothing the phone shows changed
            }
            (!send_or_drop(ws, tabs_msg(tabs, view.as_ref())).await).then_some(Exit::Failed("send"))
        }
        "Target.targetDestroyed" => {
            let id = e.params["targetId"].as_str()?;
            let known = tabs.len();
            tabs.retain(|t| t.id != id);
            if tabs.len() == known {
                return None; // not a page: nothing to show
            }
            // The viewed tab's session died with it: no stop, no detach.
            if view.as_ref().is_some_and(|v| v.target == id) {
                *view = None;
            }
            (!send_or_drop(ws, tabs_msg(tabs, view.as_ref())).await).then_some(Exit::Failed("send"))
        }
        _ => None,
    }
}

/// Leave the old tab (if any) and stream `id`. Input stays off until the new
/// tab's first frame, because the viewport went with the old view.
async fn switch_tab(cdp: &Cdp, ws: &mut WebSocket, tabs: &[Tab], view: &mut Option<View>, id: &str) -> Option<Exit> {
    if !tabs.iter().any(|t| t.id == id) || view.as_ref().is_some_and(|v| v.target == id) {
        return None;
    }
    if let Some(old) = view.take() {
        stop_view(cdp, &old).await;
    }
    match start_view(cdp, id).await {
        Ok(v) => *view = Some(v),
        Err(e) => return Some(failed(e)),
    }
    (!send_or_drop(ws, tabs_msg(tabs, view.as_ref())).await).then_some(Exit::Failed("send"))
}

async fn back(cdp: &Cdp, session: Option<&str>) -> Result<(), &'static str> {
    let h = cdp.call("Page.getNavigationHistory", json!({}), session).await?;
    let previous = (h["currentIndex"].as_u64().and_then(|i| i.checked_sub(1)))
        .and_then(|i| h["entries"][i as usize]["id"].as_u64());
    if let Some(entry) = previous {
        cdp.call("Page.navigateToHistoryEntry", json!({"entryId": entry}), session).await?;
    }
    Ok(())
}

/// Points are clamped to the page; text is one key press per character, never
/// one insert, so one-box-per-digit fields fill.
async fn send_input(cdp: &Cdp, session: Option<&str>, vp: &Viewport, msg: PhoneMsg) -> Result<(), &'static str> {
    let mouse = |p: Value| cdp.call("Input.dispatchMouseEvent", p, session);
    let key = |p: Value| cdp.call("Input.dispatchKeyEvent", p, session);
    match msg {
        PhoneMsg::Tap(x, y) => {
            let (x, y) = clamp_point(x, y, vp);
            mouse(json!({"type": "mouseMoved", "x": x, "y": y})).await?;
            for kind in ["mousePressed", "mouseReleased"] {
                mouse(json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1})).await?;
            }
        }
        PhoneMsg::Scroll { x, y, dx, dy } => {
            let (x, y) = clamp_point(x, y, vp);
            mouse(json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy})).await?;
        }
        PhoneMsg::Text(t) => {
            for c in t.chars() {
                for ev in char_events(c) {
                    key(ev).await?;
                }
            }
        }
        PhoneMsg::Key(k) => {
            for ev in key_events(k).unwrap_or_default() {
                key(ev).await?;
            }
        }
        PhoneMsg::Tab(_) | PhoneMsg::Back | PhoneMsg::Reload => {}
    }
    Ok(())
}

async fn on_phone(cdp: &Cdp, ws: &mut WebSocket, tabs: &[Tab], view: &mut Option<View>, msg: PhoneMsg) -> Option<Exit> {
    if let PhoneMsg::Tab(id) = &msg {
        return switch_tab(cdp, ws, tabs, view, id).await;
    }
    // With no tab, navigation and input have nowhere to go.
    let v = view.as_ref()?;
    let session = Some(v.session.as_str());
    let done = match msg {
        PhoneMsg::Back => back(cdp, session).await,
        PhoneMsg::Reload => cdp.call("Page.reload", json!({}), session).await.map(drop),
        // Input needs the page size of a frame the phone has been shown.
        input => match &v.viewport {
            Some(vp) => send_input(cdp, session, vp, input).await,
            None => Ok(()),
        },
    };
    done.err().map(failed)
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
    let mut ping = ping_interval();
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
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Some(m) = parse_phone(t.as_str()) {
                            if let Some(exit) = on_phone(cdp, ws, tabs, view, m).await { return exit }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Exit::Phone,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn run(port: u16, ws: &mut WebSocket) -> Exit {
    let url = match browser_ws_url(port).await {
        Ok(u) => u,
        Err(e) => return Exit::NotRunning(e),
    };
    let (cdp, mut events) = match Cdp::connect(&url).await {
        Ok(c) => c,
        Err(e) => return Exit::NotRunning(e),
    };
    // Discover first, so a tab change cannot fall between the snapshot and the
    // subscription; early events queue and are applied after the list is sent.
    if let Err(e) = cdp.call("Target.setDiscoverTargets", json!({"discover": true}), None).await {
        return failed(e);
    }
    let listed = match cdp.call("Target.getTargets", json!({}), None).await {
        Ok(v) => v,
        Err(e) => return failed(e),
    };
    let mut tabs: Vec<Tab> = listed["targetInfos"].as_array().map(|a| a.iter().filter_map(tab_of).collect()).unwrap_or_default();
    let mut view = None;
    if let Some(t) = initial_tab(&tabs, &page_order(port).await) {
        match start_view(&cdp, &t.id).await {
            Ok(v) => view = Some(v),
            Err(e) => return failed(e),
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
        close_and_drain(&mut ws, code, reason).await;
    }
    ctx.host.log.push(format!("browser close {name}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::browser::MAP_FILE;
    use crate::collect::cdp::fake::{fake_browser, Call, FakeBrowser};
    use crate::collect::term::tests::{eventually, have_tmux, status_of, tempdir, Session, Ws};
    use crate::http::serve::{serve, tests::cfg_for, Bound};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message as WsMsg};

    const WAIT: Duration = Duration::from_secs(5);
    const CANARIES: [&str; 2] = ["CANARY", "typed-text"];
    /// The only CDP methods cdash-agent may call.
    const ALLOWED: [&str; 13] = [
        "Target.getTargets", "Target.setDiscoverTargets", "Target.attachToTarget", "Target.detachFromTarget",
        "Page.enable", "Page.startScreencast", "Page.stopScreencast", "Page.screencastFrameAck",
        "Page.getNavigationHistory", "Page.navigateToHistoryEntry", "Page.reload",
        "Input.dispatchMouseEvent", "Input.dispatchKeyEvent",
    ];

    struct Rig {
        b: Bound,
        fake: FakeBrowser,
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
        let fake = fake_browser(pages).await;
        let b = serve(cfg_for(claude.clone())).await.unwrap();
        let rig = Rig { name: s.0.clone(), b, fake, claude, work, _s: s };
        rig.remap(Some(rig.fake.port));
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

    async fn call_until(fake: &mut FakeBrowser, method: &str) -> (Value, Option<String>) {
        tokio::time::timeout(WAIT, async {
            loop {
                let (m, p, s) = fake.calls.recv().await.expect("fake browser gone");
                if m == method {
                    return (p, s);
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{method} was never called"))
    }

    /// Everything the bridge calls until it has been quiet for `settle`.
    async fn drain_calls(fake: &mut FakeBrowser, settle: Duration) -> Vec<(String, Value, Option<String>)> {
        let mut out = Vec::new();
        while let Ok(Some(c)) = tokio::time::timeout(settle, fake.calls.recv()).await {
            out.push(c);
        }
        out
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
        assert_eq!(status_of(connect(r.b.addr, &q, Some("https://CANARY.example")).await), 403);
        assert_eq!(status_of(connect(r.b.addr, "name=other-CANARY", None).await), 400);
        assert_eq!(status_of(connect(r.b.addr, "name=cdash-missing", None).await), 404);
        r.remap(None);
        assert_eq!(status_of(connect(r.b.addr, &q, None).await), 404, "a session in an unmapped folder");
        let log = r.log();
        for line in ["browser refused: origin", "browser refused: name", "browser refused: session", "browser refused: mapping"] {
            assert!(log.iter().any(|l| l == line), "missing {line:?} in {log:?}");
        }
        r.assert_no_canary();
    }

    #[tokio::test]
    async fn frame_is_relayed_then_acked() {
        let Some(mut r) = rig("frame", &[("P1", "https://a.example/login?x=1", "A")]).await else { return };
        let mut ws = r.open().await;
        let tabs = text(&mut ws).await;
        assert_eq!(tabs["active"], "P1");
        assert_eq!(tabs["tabs"], json!([{"id": "P1", "title": "A", "origin": "https://a.example"}]));
        let (p, s) = call_until(&mut r.fake, "Page.startScreencast").await;
        assert_eq!((p["format"].as_str(), p["quality"].as_u64(), p["maxWidth"].as_u64(), s.as_deref()), (Some("jpeg"), Some(60), Some(1280), Some("S-P1")));

        r.fake.events.send(frame_event("S-P1", "SlBFRw==", 7, 1000.0, 600.0)).unwrap();
        let meta = text(&mut ws).await;
        assert_eq!(meta, json!({"frame": {"deviceWidth": 1000.0, "deviceHeight": 600.0}}));
        assert_eq!(next_msg(&mut ws).await, WsMsg::binary(b"JPEG".to_vec()));
        let (p, s) = call_until(&mut r.fake, "Page.screencastFrameAck").await;
        assert_eq!((p["sessionId"].as_u64(), s.as_deref()), (Some(7), Some("S-P1")));
    }

    #[tokio::test]
    async fn a_frame_with_bad_metadata_is_acked_but_never_relayed() {
        let Some(mut r) = rig("badframe", &[("P1", "https://a.example/", "A")]).await else { return };
        let mut ws = r.open().await;
        text(&mut ws).await;
        call_until(&mut r.fake, "Page.startScreencast").await;
        for (i, (w, h)) in [(0.0, 600.0), (1000.0, -1.0)].into_iter().enumerate() {
            r.fake.events.send(frame_event("S-P1", "QkFE", 100 + i as u64, w, h)).unwrap();
        }
        r.fake.events.send(frame_event("S-P1", "!!not base64!!", 102, 1000.0, 600.0)).unwrap();
        r.fake.events.send(frame_event("S-OLD", "T0xE", 103, 1000.0, 600.0)).unwrap();
        r.fake.events.send(frame_event("S-P1", "R09PRA==", 104, 1000.0, 600.0)).unwrap();
        assert_eq!(text(&mut ws).await["frame"]["deviceWidth"], 1000.0);
        assert_eq!(next_msg(&mut ws).await, WsMsg::binary(b"GOOD".to_vec()), "only the valid frame reaches the phone");
        let acks: Vec<u64> = drain_calls(&mut r.fake, Duration::from_millis(300)).await.into_iter()
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
        r.fake.drop_socket.notify_one();
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
        r.fake.replies.lock().unwrap().insert("Page.startScreencast".into(), err(-32000));
        assert_eq!(close_of(&mut r.open().await).await, (1011, "Browser connection failed".to_string()));
        eventually("failure is logged", || r.log().contains(&format!("browser failed {}: protocol", r.name))).await;
        r.assert_no_canary();
    }

    #[tokio::test]
    async fn missing_cdp_method_closes_4004() {
        let Some(r) = rig("unsupported", &[("P1", "https://a.example/", "A")]).await else { return };
        r.fake.replies.lock().unwrap().insert(
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
                r.fake.replies.lock().unwrap().insert(
                    "Page.stopScreencast".into(),
                    Some(json!({"error": {"code": -32000, "message": "URL_CANARY"}})),
                );
            }
            let mut ws = r.open().await;
            text(&mut ws).await;
            ws.send(WsMsg::Close(None)).await.unwrap();
            eventually("the CDP socket closes", || r.fake.connections.load(Ordering::SeqCst) == 0).await;
            let calls = drain_calls(&mut r.fake, Duration::from_millis(300)).await;
            let find = |m: &str| calls.iter().find(|(c, ..)| c == m);
            assert_eq!(find("Page.stopScreencast").and_then(|c| c.2.clone()).as_deref(), Some("S-P1"), "{tag}");
            assert_eq!(find("Target.detachFromTarget").and_then(|c| c.1["sessionId"].as_str().map(str::to_string)).as_deref(), Some("S-P1"), "{tag}: detach even when stop failed");
            for (m, ..) in &calls {
                assert!(ALLOWED.contains(&m.as_str()), "{m} is not an allowed CDP method");
            }
            r.assert_no_canary();
        }
    }

    #[tokio::test]
    async fn a_web_page_is_viewed_before_the_browsers_own_pages() {
        // Headed Edge 157 lists its sync dialog as a page, first; it never paints.
        let pages = [("D", "edge://sync-confirmation-dialog/", "We are now syncing"), ("P1", "https://a.example/", "A")];
        let Some(mut r) = rig("webfirst", &pages).await else { return };
        let mut ws = r.open().await;
        let tabs = text(&mut ws).await;
        assert_eq!(tabs["active"], "P1", "{tabs}");
        assert_eq!(tabs["tabs"].as_array().unwrap().len(), 2, "the browser's own page stays pickable");
        let (p, _) = call_until(&mut r.fake, "Target.attachToTarget").await;
        assert_eq!(p["targetId"], "P1");
    }

    #[tokio::test]
    async fn no_pages_waits_for_user_selection() {
        let Some(mut r) = rig("nopages", &[]).await else { return };
        let mut ws = r.open().await;
        assert_eq!(text(&mut ws).await, json!({"tabs": [], "active": null}));
        r.fake.events.send(page_event("Target.targetCreated", "P9", "https://z.example/")).unwrap();
        assert_eq!(text(&mut ws).await, json!({"tabs": [{"id": "P9", "title": "T", "origin": "https://z.example"}], "active": null}));
        let calls = drain_calls(&mut r.fake, Duration::from_millis(300)).await;
        assert!(calls.iter().all(|(m, ..)| m != "Target.attachToTarget" && m != "Page.startScreencast"), "{calls:?}");
    }

    async fn send(ws: &mut Ws, v: Value) {
        ws.send(WsMsg::text(v.to_string())).await.unwrap();
    }

    fn only<'a>(calls: &'a [Call], method: &str) -> Vec<&'a Call> {
        calls.iter().filter(|(m, ..)| m == method).collect()
    }

    const SETTLE: Duration = Duration::from_millis(250);

    /// Phone connected, first frame (1000x600 on P1) shown, setup calls drained.
    async fn viewing(tag: &str, pages: &[(&str, &str, &str)]) -> Option<(Rig, Ws)> {
        let mut r = rig(tag, pages).await?;
        let mut ws = r.open().await;
        text(&mut ws).await;
        call_until(&mut r.fake, "Page.startScreencast").await;
        r.fake.events.send(frame_event("S-P1", "SlBFRw==", 1, 1000.0, 600.0)).unwrap();
        text(&mut ws).await;
        next_msg(&mut ws).await;
        drain_calls(&mut r.fake, SETTLE).await;
        Some((r, ws))
    }

    #[tokio::test]
    async fn input_before_first_frame_is_dropped() {
        let Some(mut r) = rig("noframe", &[("P1", "https://a.example/", "A")]).await else { return };
        let mut ws = r.open().await;
        text(&mut ws).await;
        call_until(&mut r.fake, "Page.startScreencast").await;
        send(&mut ws, json!({"tap": [5, 5]})).await;
        let early = drain_calls(&mut r.fake, SETTLE).await;
        assert!(only(&early, "Input.dispatchMouseEvent").is_empty(), "{early:?}");
        r.fake.events.send(frame_event("S-P1", "SlBFRw==", 1, 1000.0, 600.0)).unwrap();
        text(&mut ws).await;
        next_msg(&mut ws).await;
        send(&mut ws, json!({"tap": [2000, -1]})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        let mouse = only(&calls, "Input.dispatchMouseEvent");
        let kinds: Vec<_> = mouse.iter().map(|c| c.1["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["mouseMoved", "mousePressed", "mouseReleased"]);
        for c in &mouse {
            assert_eq!((c.1["x"].as_f64(), c.1["y"].as_f64(), c.2.as_deref()), (Some(1000.0), Some(0.0), Some("S-P1")));
        }
        assert_eq!((mouse[1].1["button"].as_str(), mouse[1].1["clickCount"].as_u64()), (Some("left"), Some(1)));
    }

    #[tokio::test]
    async fn scroll_becomes_a_clamped_wheel_event() {
        let Some((mut r, mut ws)) = viewing("scroll", &[("P1", "https://a.example/", "A")]).await else { return };
        send(&mut ws, json!({"scroll": [-3, 700, 4, -25]})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        let wheel = only(&calls, "Input.dispatchMouseEvent");
        assert_eq!(wheel.len(), 1);
        let p = &wheel[0].1;
        assert_eq!((p["type"].as_str(), p["x"].as_f64(), p["y"].as_f64(), p["deltaX"].as_f64(), p["deltaY"].as_f64()), (Some("mouseWheel"), Some(0.0), Some(600.0), Some(4.0), Some(-25.0)));
    }

    #[tokio::test]
    async fn text_becomes_key_presses() {
        let Some((mut r, mut ws)) = viewing("text", &[("P1", "https://a.example/", "A")]).await else { return };
        send(&mut ws, json!({"text": "a$"})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        let keys: Vec<_> = only(&calls, "Input.dispatchKeyEvent").iter().map(|c| (c.1["type"].as_str().unwrap().to_string(), c.1["text"].as_str().map(str::to_string), c.1["windowsVirtualKeyCode"].as_u64())).collect();
        assert_eq!(keys, vec![
            ("keyDown".to_string(), Some("a".to_string()), Some(65)), ("keyUp".to_string(), None, Some(65)),
            ("keyDown".to_string(), Some("$".to_string()), None), ("keyUp".to_string(), None, None),
        ]);
        assert!(calls.iter().all(|(m, ..)| m != "Input.insertText"), "text is never one insert");
    }

    #[tokio::test]
    async fn backspace_has_vk_8() {
        let Some((mut r, mut ws)) = viewing("bksp", &[("P1", "https://a.example/", "A")]).await else { return };
        send(&mut ws, json!({"key": "Backspace"})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        let keys = only(&calls, "Input.dispatchKeyEvent");
        let seen: Vec<_> = keys.iter().map(|c| (c.1["type"].as_str().unwrap(), c.1["windowsVirtualKeyCode"].as_u64())).collect();
        assert_eq!(seen, [("rawKeyDown", Some(8)), ("keyUp", Some(8))]);
    }

    #[tokio::test]
    async fn tab_switch_and_close() {
        let Some((mut r, mut ws)) = viewing("tabs", &[("P1", "https://a.example/", "A"), ("P2", "https://b.example/", "B")]).await else { return };
        send(&mut ws, json!({"tab": "nope"})).await;
        send(&mut ws, json!({"tab": "P1"})).await;
        let none = drain_calls(&mut r.fake, SETTLE).await;
        assert!(none.is_empty(), "an unlisted tab and the tab already shown make no calls: {none:?}");

        send(&mut ws, json!({"tab": "P2"})).await;
        let tabs = text(&mut ws).await;
        assert_eq!(tabs["active"], "P2");
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        let attach = only(&calls, "Target.attachToTarget");
        assert_eq!(attach.len(), 1);
        assert_eq!(attach[0].1["targetId"], "P2");
        assert_eq!(only(&calls, "Page.stopScreencast")[0].2.as_deref(), Some("S-P1"), "the old view is stopped");

        r.fake.events.send(page_event("Target.targetCreated", "P3", "https://c.example/")).unwrap();
        let tabs = text(&mut ws).await;
        assert_eq!((tabs["active"].clone(), tabs["tabs"].as_array().unwrap().len()), (json!("P2"), 3));

        r.fake.events.send(frame_event("S-P2", "SlBFRw==", 2, 500.0, 300.0)).unwrap();
        text(&mut ws).await;
        next_msg(&mut ws).await;
        r.fake.events.send(json!({"method": "Target.targetDestroyed", "params": {"targetId": "P2"}})).unwrap();
        let tabs = text(&mut ws).await;
        assert_eq!(tabs["active"], Value::Null);
        assert_eq!(tabs["tabs"].as_array().unwrap().len(), 2);
        drain_calls(&mut r.fake, SETTLE).await;
        send(&mut ws, json!({"text": "x"})).await;
        let after = drain_calls(&mut r.fake, SETTLE).await;
        assert!(after.is_empty(), "no input once the viewed tab is gone: {after:?}");
    }

    #[tokio::test]
    async fn back_uses_the_previous_entry_id() {
        let Some((mut r, mut ws)) = viewing("back", &[("P1", "https://a.example/", "A")]).await else { return };
        send(&mut ws, json!({"nav": "back"})).await;
        send(&mut ws, json!({"nav": "reload"})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        let nav = only(&calls, "Page.navigateToHistoryEntry");
        assert_eq!((nav.len(), nav[0].1["entryId"].as_u64(), nav[0].2.as_deref()), (1, Some(10), Some("S-P1")));
        assert_eq!(only(&calls, "Page.reload")[0].2.as_deref(), Some("S-P1"));
    }

    #[tokio::test]
    async fn back_at_the_first_entry_does_nothing() {
        let Some((mut r, mut ws)) = viewing("back0", &[("P1", "https://a.example/", "A")]).await else { return };
        r.fake.replies.lock().unwrap().insert(
            "Page.getNavigationHistory".into(),
            Some(json!({"result": {"currentIndex": 0, "entries": [{"id": 10}]}})),
        );
        send(&mut ws, json!({"nav": "back"})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        assert_eq!(only(&calls, "Page.getNavigationHistory").len(), 1, "Back looked at the history: {calls:?}");
        assert!(only(&calls, "Page.navigateToHistoryEntry").is_empty(), "{calls:?}");
    }

    #[tokio::test]
    async fn old_frames_cannot_enable_input_after_tab_switch() {
        let Some((mut r, mut ws)) = viewing("stale", &[("P1", "https://a.example/", "A"), ("P2", "https://b.example/", "B")]).await else { return };
        send(&mut ws, json!({"tab": "P2"})).await;
        text(&mut ws).await;
        drain_calls(&mut r.fake, SETTLE).await;

        // A queued frame from the old session, then input: nothing.
        r.fake.events.send(frame_event("S-P1", "T0xE", 50, 1000.0, 600.0)).unwrap();
        tokio::time::sleep(SETTLE).await;
        send(&mut ws, json!({"tap": [5, 5]})).await;
        // A frame for the new tab with an unusable size does not enable input either.
        r.fake.events.send(frame_event("S-P2", "QkFE", 51, 0.0, 300.0)).unwrap();
        tokio::time::sleep(SETTLE).await;
        send(&mut ws, json!({"text": "x"})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        assert!(calls.iter().all(|(m, ..)| !m.starts_with("Input.")), "input reached the browser: {calls:?}");
        assert!(only(&calls, "Page.screencastFrameAck").iter().all(|c| c.2.as_deref() == Some("S-P2")), "the old session's frame is not acked");

        // Only a valid frame from the new tab sets the viewport, and clamping uses it.
        r.fake.events.send(frame_event("S-P2", "SlBFRw==", 52, 500.0, 300.0)).unwrap();
        text(&mut ws).await;
        next_msg(&mut ws).await;
        send(&mut ws, json!({"tap": [9999, 9999]})).await;
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        let mouse = only(&calls, "Input.dispatchMouseEvent");
        assert_eq!(mouse.len(), 3);
        for c in mouse {
            assert_eq!((c.1["x"].as_f64(), c.1["y"].as_f64(), c.2.as_deref()), (Some(500.0), Some(300.0), Some("S-P2")));
        }
    }

    #[tokio::test]
    async fn closed_tab_can_be_replaced() {
        let Some((mut r, mut ws)) = viewing("closed", &[("P1", "https://a.example/", "A"), ("P2", "https://b.example/", "B")]).await else { return };
        r.fake.events.send(json!({"method": "Target.targetDestroyed", "params": {"targetId": "P1"}})).unwrap();
        assert_eq!(text(&mut ws).await["active"], Value::Null);
        drain_calls(&mut r.fake, SETTLE).await;
        send(&mut ws, json!({"nav": "back"})).await;
        send(&mut ws, json!({"nav": "reload"})).await;
        let idle = drain_calls(&mut r.fake, SETTLE).await;
        assert!(idle.is_empty(), "back/reload with no active tab: {idle:?}");

        send(&mut ws, json!({"tab": "P2"})).await;
        assert_eq!(text(&mut ws).await["active"], "P2");
        let calls = drain_calls(&mut r.fake, SETTLE).await;
        assert_eq!(only(&calls, "Target.attachToTarget")[0].1["targetId"], "P2");
        assert!(only(&calls, "Page.stopScreencast").is_empty() && only(&calls, "Target.detachFromTarget").is_empty(), "the destroyed session is not called: {calls:?}");
    }

    #[tokio::test]
    async fn non_page_targets_do_not_disturb_the_list() {
        let Some((r, mut ws)) = viewing("nonpage", &[("P1", "https://a.example/", "A")]).await else { return };
        r.fake.events.send(json!({"method": "Target.targetCreated", "params": {"targetInfo": {"targetId": "W1", "type": "service_worker", "url": "https://a.example/sw.js"}}})).unwrap();
        r.fake.events.send(json!({"method": "Target.targetDestroyed", "params": {"targetId": "W1"}})).unwrap();
        r.fake.events.send(json!({"method": "Target.targetInfoChanged", "params": {"targetInfo": {"targetId": "P1", "type": "page", "url": "https://a.example/", "title": "A"}}})).unwrap();
        r.fake.events.send(page_event("Target.targetInfoChanged", "P1", "https://a.example/next")).unwrap();
        let tabs = text(&mut ws).await;
        assert_eq!(tabs["tabs"].as_array().unwrap().len(), 1, "the first list update is P1's own: {tabs}");
        assert_eq!(tabs["tabs"][0]["title"], "T", "an update that changes nothing shown is not sent: {tabs}");
        assert_eq!(tabs["active"], "P1");
    }

    #[tokio::test]
    async fn input_failures_never_expose_canaries() {
        let Some((r, mut ws)) = viewing("inputfail", &[("P1", "https://a.example/", "A")]).await else { return };
        r.fake.replies.lock().unwrap().insert(
            "Input.dispatchKeyEvent".into(),
            Some(json!({"error": {"code": -32000, "message": "https://secret.example/?URL_CANARY typed-text"}})),
        );
        send(&mut ws, json!({"text": "typed-text"})).await;
        assert_eq!(close_of(&mut ws).await, (1011, "Browser connection failed".to_string()));
        eventually("failure is logged", || r.log().contains(&format!("browser failed {}: protocol", r.name))).await;
        r.assert_no_canary();
    }
}

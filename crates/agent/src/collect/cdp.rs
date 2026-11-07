//! A minimal CDP client over the browser-level WebSocket. It sends whatever the
//! caller asks and reports only fixed error categories; the bridge owns the
//! allowlist of methods.

use super::browser::discover_json;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the writer waits for the close handshake after the client is dropped.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

type Reply = Result<Value, &'static str>;
/// `None` once the socket is gone, so a late call fails at once instead of
/// waiting out its deadline.
type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Reply>>>>>;

fn lock(p: &Pending) -> MutexGuard<'_, Option<HashMap<u64, oneshot::Sender<Reply>>>> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug)]
pub struct Event {
    pub method: String,
    pub params: Value,
    pub session: Option<String>,
}

pub struct Cdp {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Cdp {
    /// The writer ends by itself once `out` is dropped (it sends a close frame,
    /// bounded by `CLOSE_GRACE`); the reader needs the abort, which also ends
    /// the event stream.
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Route one incoming frame: a reply by `id`, or an event by `method`. An error
/// reply keeps only its code; the message can carry URLs, titles or input.
fn route(pending: &Pending, events: &mpsc::UnboundedSender<Event>, text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    if let Some(id) = v["id"].as_u64() {
        let tx = lock(pending).as_mut().and_then(|m| m.remove(&id));
        let reply = match v.get("error") {
            Some(e) if e["code"].as_i64() == Some(-32601) => Err("unsupported"),
            Some(_) => Err("protocol"),
            None => Ok(v.get("result").cloned().unwrap_or_else(|| json!({}))),
        };
        if let Some(tx) = tx {
            let _ = tx.send(reply);
        }
    } else if let Some(method) = v["method"].as_str() {
        let _ = events.send(Event {
            method: method.to_string(),
            params: v.get("params").cloned().unwrap_or(Value::Null),
            session: v["sessionId"].as_str().map(str::to_string),
        });
    }
}

impl Cdp {
    /// `ws_url` must come from `browser::browser_ws_url`. Anything that is not
    /// plain `ws://127.0.0.1:` is refused here too. `connect_async` sends no
    /// `Origin` header, and none is added.
    pub async fn connect(ws_url: &str) -> Result<(Cdp, mpsc::UnboundedReceiver<Event>), String> {
        if !ws_url.starts_with("ws://127.0.0.1:") {
            return Err("protocol".to_string());
        }
        let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(ws_url))
            .await
            .map_err(|_| "timeout".to_string())?
            .map_err(|_| "connect".to_string())?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (events, event_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));

        tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                if sink.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
            let _ = tokio::time::timeout(CLOSE_GRACE, sink.close()).await;
        });

        let reader = tokio::spawn({
            let pending = Arc::clone(&pending);
            async move {
                loop {
                    match stream.next().await {
                        Some(Ok(Message::Text(t))) => route(&pending, &events, t.as_str()),
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => {}
                    }
                }
                // Socket gone: fail every waiting call. `events` drops with this
                // task, which ends the event stream.
                if let Some(map) = lock(&pending).take() {
                    for tx in map.into_values() {
                        let _ = tx.send(Err("connect"));
                    }
                }
            }
        });

        Ok((Cdp { out, pending, next: AtomicU64::new(1), reader }, event_rx))
    }

    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        self.call_within(method, params, session, CALL_TIMEOUT).await
    }

    async fn call_within(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        limit: Duration,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        match lock(&self.pending).as_mut() {
            Some(map) => map.insert(id, tx),
            None => return Err("connect".to_string()),
        };
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = s.into();
        }
        let result = if self.out.send(msg.to_string()).is_err() {
            Err("connect")
        } else {
            match tokio::time::timeout(limit, rx).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(_)) => Err("connect"),
                Err(_) => Err("timeout"),
            }
        };
        if let Some(map) = lock(&self.pending).as_mut() {
            map.remove(&id);
        }
        result.map_err(str::to_string)
    }
}

/// Ids of the browser's `page` targets, in `/json/list` order. That order is a
/// hint, not guaranteed activity order. Empty on any error.
pub async fn page_order(port: u16) -> Vec<String> {
    let Ok(list) = discover_json(port, "/json/list").await else { return Vec::new() };
    list.as_array()
        .map(|a| {
            a.iter()
                .filter(|t| t["type"] == "page")
                .filter_map(|t| t["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A loopback stand-in for a Chromium browser: `/json/version`, `/json/list`
/// and one CDP WebSocket. Shared by the bridge tests. The name is only a
/// fixture name; nothing here requires an Edge product string.
#[cfg(test)]
pub(crate) mod fake {
    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, Notify};

    type Call = (String, Value, Option<String>);

    pub(crate) struct FakeEdge {
        pub port: u16,
        /// Every CDP call received: (method, params, sessionId).
        pub calls: mpsc::UnboundedReceiver<Call>,
        /// A value sent here is written to the CDP socket as-is.
        pub events: mpsc::UnboundedSender<Value>,
        /// `notify_one()` closes the CDP socket.
        pub drop_socket: Arc<Notify>,
        /// Per-method override: `Some({"result":..})` or `Some({"error":..})`
        /// (the incoming id is added); `None` withholds the reply.
        pub replies: Arc<Mutex<HashMap<String, Option<Value>>>>,
        /// CDP sockets currently open, to assert the client really closed.
        pub connections: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct Shared {
        port: u16,
        pages: Arc<Vec<(String, String, String)>>,
        calls: mpsc::UnboundedSender<Call>,
        events: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>>,
        drop_socket: Arc<Notify>,
        replies: Arc<Mutex<HashMap<String, Option<Value>>>>,
        connections: Arc<AtomicUsize>,
    }

    struct Live(Arc<AtomicUsize>);
    impl Drop for Live {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub(crate) async fn fake_edge(pages: &[(&str, &str, &str)]) -> FakeEdge {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (calls_tx, calls) = mpsc::unbounded_channel();
        let (events, events_rx) = mpsc::unbounded_channel();
        let drop_socket = Arc::new(Notify::new());
        let replies = Arc::new(Mutex::new(HashMap::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let state = Shared {
            port,
            pages: Arc::new(pages.iter().map(|(i, u, t)| (i.to_string(), u.to_string(), t.to_string())).collect()),
            calls: calls_tx,
            events: Arc::new(tokio::sync::Mutex::new(events_rx)),
            drop_socket: Arc::clone(&drop_socket),
            replies: Arc::clone(&replies),
            connections: Arc::clone(&connections),
        };
        let app = axum::Router::new()
            .route("/json/version", axum::routing::get(version))
            .route("/json/list", axum::routing::get(list))
            .route("/devtools/browser/fake", axum::routing::get(ws))
            .with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        FakeEdge { port, calls, events, drop_socket, replies, connections }
    }

    async fn version(State(st): State<Shared>) -> axum::Json<Value> {
        axum::Json(json!({
            "Browser": "Chrome/0.0.0.0",
            "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/browser/fake", st.port),
        }))
    }

    async fn list(State(st): State<Shared>) -> axum::Json<Value> {
        axum::Json(Value::Array(
            st.pages.iter().map(|(id, url, title)| json!({"id": id, "type": "page", "url": url, "title": title})).collect(),
        ))
    }

    /// Chromium refuses a cross-origin upgrade; so does the fake, which keeps
    /// the client honest about sending no `Origin`.
    async fn ws(State(st): State<Shared>, headers: HeaderMap, up: WebSocketUpgrade) -> axum::response::Response {
        if headers.contains_key("origin") {
            return StatusCode::FORBIDDEN.into_response();
        }
        up.on_upgrade(move |socket| session(st, socket))
    }

    fn default_reply(st: &Shared, method: &str, params: &Value) -> Value {
        match method {
            "Target.getTargets" => json!({"result": {"targetInfos": st.pages.iter().map(|(id, url, title)|
                json!({"targetId": id, "type": "page", "url": url, "title": title})).collect::<Vec<_>>()}}),
            "Target.attachToTarget" => json!({"result": {"sessionId": format!("S-{}", params["targetId"].as_str().unwrap_or(""))}}),
            "Page.getNavigationHistory" => json!({"result": {"currentIndex": 1, "entries": [{"id": 10}, {"id": 11}]}}),
            _ => json!({"result": {}}),
        }
    }

    async fn session(st: Shared, mut socket: WebSocket) {
        st.connections.fetch_add(1, Ordering::SeqCst);
        let _live = Live(Arc::clone(&st.connections));
        let mut events = st.events.lock().await;
        loop {
            tokio::select! {
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else { continue };
                        let method = v["method"].as_str().unwrap_or("").to_string();
                        let params = v.get("params").cloned().unwrap_or(Value::Null);
                        let _ = st.calls.send((method.clone(), params.clone(), v["sessionId"].as_str().map(str::to_string)));
                        let over = st.replies.lock().unwrap().get(&method).cloned();
                        let reply = match over {
                            Some(None) => continue,
                            Some(Some(r)) => r,
                            None => default_reply(&st, &method, &params),
                        };
                        let mut reply = reply;
                        reply["id"] = v["id"].clone();
                        if socket.send(Message::Text(reply.to_string().into())).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                ev = events.recv() => match ev {
                    Some(e) => { if socket.send(Message::Text(e.to_string().into())).await.is_err() { break; } }
                    None => break,
                },
                _ = st.drop_socket.notified() => {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::browser::browser_ws_url;
    use crate::collect::browser::tests::serve;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(5);

    async fn next_event(ev: &mut mpsc::UnboundedReceiver<Event>) -> Option<Event> {
        tokio::time::timeout(WAIT, ev.recv()).await.expect("event stream neither yielded nor ended")
    }

    async fn until(mut done: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + WAIT;
        while Instant::now() < end {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        done()
    }

    #[tokio::test]
    async fn calls_events_and_close() {
        let mut edge = fake::fake_edge(&[("P1", "https://a.example/", "A")]).await;
        let url = browser_ws_url(edge.port).await.unwrap();
        let (cdp, mut ev) = Cdp::connect(&url).await.unwrap();
        let r = cdp.call("Target.attachToTarget", json!({"targetId":"P1","flatten":true}), None).await.unwrap();
        assert_eq!(r["sessionId"], "S-P1");
        let (m, p, s) = edge.calls.recv().await.unwrap();
        assert_eq!((m.as_str(), p["targetId"].as_str(), s), ("Target.attachToTarget", Some("P1"), None));
        edge.events.send(json!({"method":"Page.screencastFrame","params":{"sessionId":7},"sessionId":"S-P1"})).unwrap();
        let e = next_event(&mut ev).await.unwrap();
        assert_eq!((e.method.as_str(), e.session.as_deref()), ("Page.screencastFrame", Some("S-P1")));
        assert_eq!(page_order(edge.port).await, vec!["P1".to_string()]);
        edge.drop_socket.notify_one();
        assert!(next_event(&mut ev).await.is_none(), "event stream ends when the browser goes away");
        assert!(cdp.call("Page.reload", json!({}), Some("S-P1")).await.is_err());
    }

    #[tokio::test]
    async fn list_discovery_never_follows_redirects() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let trap = serve(axum::Router::new().fallback(move || {
            let h = h.clone();
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!([{"id": "TRAP", "type": "page"}]))
            }
        }))
        .await;
        let loc = format!("http://127.0.0.1:{trap}/json/list");
        let port = serve(axum::Router::new().route(
            "/json/list",
            axum::routing::get(move || {
                let loc = loc.clone();
                async move { (axum::http::StatusCode::FOUND, [(axum::http::header::LOCATION, loc)]) }
            }),
        ))
        .await;
        assert!(page_order(port).await.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 0, "the redirect target received a request");
    }

    #[tokio::test]
    async fn cdp_errors_are_fixed_categories() {
        let edge = fake::fake_edge(&[]).await;
        let (cdp, _ev) = Cdp::connect(&browser_ws_url(edge.port).await.unwrap()).await.unwrap();
        let canary = "https://secret.example/?token=URL_CANARY TITLE_CANARY typed-text";
        {
            let mut replies = edge.replies.lock().unwrap();
            replies.insert("Page.enable".into(), Some(json!({"error": {"code": -32601, "message": canary}})));
            replies.insert("Page.reload".into(), Some(json!({"error": {"code": -32000, "message": canary}})));
            replies.insert("Page.stopScreencast".into(), None);
        }

        assert_eq!(cdp.call("Page.enable", json!({}), None).await, Err("unsupported".to_string()));
        assert_eq!(cdp.call("Page.reload", json!({}), None).await, Err("protocol".to_string()));
        let hung = cdp.call_within("Page.stopScreencast", json!({}), None, Duration::from_millis(150)).await;
        assert_eq!(hung, Err("timeout".to_string()));
        assert!(cdp.call("Target.getTargets", json!({}), None).await.is_ok(), "a timed-out call leaves the client usable");
    }

    #[tokio::test]
    async fn dropping_cdp_closes_socket_and_tasks() {
        let edge = fake::fake_edge(&[]).await;
        let (cdp, mut ev) = Cdp::connect(&browser_ws_url(edge.port).await.unwrap()).await.unwrap();
        cdp.call("Target.getTargets", json!({}), None).await.unwrap();
        assert_eq!(edge.connections.load(Ordering::SeqCst), 1);
        drop(cdp);
        assert!(next_event(&mut ev).await.is_none(), "event stream ends when the client is dropped");
        assert!(until(|| edge.connections.load(Ordering::SeqCst) == 0).await, "the peer saw the socket close");
    }

    #[tokio::test]
    async fn a_pending_call_fails_when_the_browser_goes_away() {
        let edge = fake::fake_edge(&[]).await;
        let (cdp, _ev) = Cdp::connect(&browser_ws_url(edge.port).await.unwrap()).await.unwrap();
        edge.replies.lock().unwrap().insert("Page.reload".into(), None);
        let started = Instant::now();
        let (r, ()) = tokio::join!(cdp.call("Page.reload", json!({}), None), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            edge.drop_socket.notify_one();
        });
        assert_eq!(r, Err("connect".to_string()));
        assert!(started.elapsed() < Duration::from_secs(3), "failed on disconnect, not on the call deadline");
    }

    #[tokio::test]
    async fn the_fake_refuses_an_origin_so_no_origin_is_proof() {
        use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
        let edge = fake::fake_edge(&[]).await;
        let mut req = browser_ws_url(edge.port).await.unwrap().into_client_request().unwrap();
        req.headers_mut().insert("origin", "https://evil.example".parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Err(tungstenite::Error::Http(res)) => assert_eq!(res.status().as_u16(), 403),
            other => panic!("expected a 403 refusal, got {:?}", other.map(|_| ())),
        }
    }

    #[tokio::test]
    async fn connect_refuses_anything_but_mapped_loopback() {
        for url in ["ws://example.com:9301/devtools/browser/x", "ws://localhost:9301/devtools/browser/x", "wss://127.0.0.1:9301/x"] {
            assert_eq!(Cdp::connect(url).await.err(), Some("protocol".to_string()), "{url}");
        }
    }
}

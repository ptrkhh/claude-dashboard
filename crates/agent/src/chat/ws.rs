//! The WebSocket between one browser and a session's hub.

use super::hub::{ClientFrame, Hub};
use crate::collect::ctx::Ctx;
use crate::collect::term::{send_or_drop, PING_EVERY};
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

/// Close code: the session ended.
const ENDED: u16 = 4000;
/// Close code: this client fell behind; reconnect with the last position.
const BEHIND: u16 = 4002;

pub async fn serve(ctx: Arc<Ctx>, hub: Hub, name: String, after: Option<u64>, epoch: Option<String>, mut ws: WebSocket) {
    ctx.host.log.push(format!("chat open {name}"));
    if let Some((code, reason)) = pump(&hub, after, epoch, &mut ws).await {
        let _ = ws.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
        // Read the browser's Close reply, as `term::bridge` does, so it sees the code.
        let drain = async { while let Some(Ok(_)) = ws.recv().await {} };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), drain).await;
    }
    ctx.host.log.push(format!("chat close {name}"));
}

fn ends_session(f: &str) -> bool {
    f.contains("\"ended\":true")
        && serde_json::from_str::<serde_json::Value>(f).is_ok_and(|v| v["t"] == "state" && v["ended"] == true)
}

async fn pump(hub: &Hub, after: Option<u64>, epoch: Option<String>, ws: &mut WebSocket) -> Option<(u16, &'static str)> {
    let Some(mut sub) = hub.subscribe(after, epoch).await else { return Some((ENDED, "session ended")) };
    let mut ended = false;
    for f in sub.backlog {
        ended = ends_session(&f);
        if !send_or_drop(ws, Message::Text(f.as_ref().into())).await {
            return None;
        }
    }
    if ended {
        return Some((ENDED, "session ended"));
    }
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heard = true;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if !std::mem::take(&mut heard) || !send_or_drop(ws, Message::Ping(axum::body::Bytes::new())).await {
                    return None;
                }
            }
            live = sub.live.recv() => match live {
                Ok(f) => {
                    if !send_or_drop(ws, Message::Text(f.as_ref().into())).await {
                        return None;
                    }
                    if ends_session(&f) {
                        return Some((ENDED, "session ended"));
                    }
                }
                Err(RecvError::Lagged(_)) => return Some((BEHIND, "fell behind")),
                Err(RecvError::Closed) => return Some((ENDED, "session ended")),
            },
            msg = ws.recv() => {
                heard = true;
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        let Ok(frame) = serde_json::from_str::<ClientFrame>(t.as_str()) else { continue };
                        if let Some(reply) = hub.command(frame).await {
                            if !send_or_drop(ws, Message::Text(reply.as_ref().into())).await {
                                return None;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::chat::claude::tests::{append, fixture_lines, fixture_with, write_session};
    use crate::chat::tmux_io::tests::tempfile_path;
    use crate::collect::ctx::Ctx;
    use crate::http::serve::{serve, tests::cfg_for};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::Value;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message as WsMsg};

    type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    struct Srv {
        addr: std::net::SocketAddr,
        ctx: Arc<Ctx>,
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let d = tempfile_path(&format!("ws-{tag}"));
        std::fs::create_dir_all(d.join("sessions")).unwrap();
        d
    }

    /// The real router over a ctx whose PATH holds only a stub tmux answering
    /// `answer`; `serve` would resolve the real PATH instead.
    fn chat_server_with_pane(tag: &str, answer: &str) -> (Srv, std::path::PathBuf) {
        let (ctx, dir) = fixture_with(&format!("ws-{tag}"), answer);
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let addr = std_listener.local_addr().unwrap();
        let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
        let app = crate::http::serve::router(Arc::clone(&ctx), Arc::clone(&cfg_for(dir.clone()).auth), None, None);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await;
        });
        (Srv { addr, ctx }, dir)
    }

    fn chat_server(tag: &str) -> (Srv, std::path::PathBuf) {
        chat_server_with_pane(tag, "4242|")
    }

    fn append_transcript(dir: &Path, lines: &str) {
        append(&dir.join("projects/-proj/sid-1.jsonl"), lines);
    }

    async fn connect(addr: std::net::SocketAddr, query: &str, origin: Option<&str>) -> Result<Ws, tungstenite::Error> {
        let mut req = format!("ws://{addr}/api/chat?{query}").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
    }

    fn status_of(r: Result<Ws, tungstenite::Error>) -> u16 {
        match r {
            Err(tungstenite::Error::Http(res)) => res.status().as_u16(),
            Err(e) => panic!("expected an HTTP refusal, got {e}"),
            Ok(_) => panic!("expected an HTTP refusal, got an open socket"),
        }
    }

    async fn next_json(ws: &mut Ws) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMsg::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
                    Some(Ok(_)) => {}
                    other => panic!("socket ended: {other:?}"),
                }
            }
        })
        .await
        .expect("no frame within 5 s")
    }

    async fn next_of(ws: &mut Ws, t: &str) -> Value {
        loop {
            let f = next_json(ws).await;
            if f["t"] == t {
                return f;
            }
        }
    }

    #[tokio::test]
    async fn refusals_happen_before_the_upgrade() {
        let b = serve(cfg_for(tempdir("chat-refuse"))).await.unwrap();
        assert_eq!(status_of(connect(b.addr, "name=cdash-x", Some("https://evil.example")).await), 403);
        assert_eq!(status_of(connect(b.addr, "name=other", None).await), 400);
        assert_eq!(status_of(connect(b.addr, "name=cdash-gone", None).await), 409, "no such pane");
    }
    #[tokio::test]
    async fn a_client_reads_history_sends_a_prompt_and_resumes_without_repeats() {
        let (b, dir) = chat_server("flow");                        // transcript = fixture lines 1..9, status idle
        let mut ws = connect(b.addr, "name=cdash-a", None).await.unwrap();
        let hello = next_json(&mut ws).await;
        assert_eq!((hello["t"].as_str(), hello["tool"].as_str(), hello["head"].as_u64()), (Some("hello"), Some("claude"), Some(5)));
        let epoch = hello["epoch"].as_str().unwrap().to_string();
        for seq in 1..=5 { assert_eq!(next_json(&mut ws).await["seq"], seq); }
        assert_eq!(next_json(&mut ws).await["t"], "state");
        ws.send(WsMsg::Text(r#"{"t":"send","cid":"c1","text":"zebra\u001b[201~"}"#.into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "ack").await["status"], "sent");
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        assert!(args.contains("paste-buffer -p -d -b cdash-chat-cdash-a -t =cdash-a:"));
        append_transcript(&dir, &fixture_lines(13..14));           // line 13, the prompt "slow job"
        assert_eq!(next_of(&mut ws, "ev").await["seq"], 6);
        drop(ws);
        let mut again = connect(b.addr, &format!("name=cdash-a&after=6&epoch={epoch}"), None).await.unwrap();
        assert_eq!(next_json(&mut again).await["reset"], false);
        assert_eq!(next_json(&mut again).await["t"], "state", "nothing is replayed");
        let log = b.ctx.host.log.lines().join("\n");
        assert!(log.contains("chat open cdash-a") && log.contains("chat send cdash-a") && !log.contains("zebra"));
    }
    #[tokio::test]
    async fn garbage_frames_are_ignored_and_a_bare_terminal_pane_has_no_chat() {
        let (b, _dir) = chat_server("garbage");
        let mut ws = connect(b.addr, "name=cdash-a", None).await.unwrap();
        ws.send(WsMsg::Text("not json".into())).await.unwrap();
        ws.send(WsMsg::Text(r#"{"t":"stop"}"#.into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "state").await["ended"], false, "still connected");
        let (t, _d) = chat_server_with_pane("bare", "4242|terminal");
        assert_eq!(status_of(connect(t.addr, "name=cdash-a", None).await), 409);
    }
}

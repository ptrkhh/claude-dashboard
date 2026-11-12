//! The Codex and OpenCode engine: NDJSON over the supervisor's unix socket.
//! The supervisor (`chat-feed.mjs`) numbers events and replays from a
//! position, so this side only remembers the last position and epoch it saw.

use super::hub::{Engine, EngineMsg};
use crate::host::log::LogBuffer;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

const ACK_WAIT: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(1);
/// Consecutive failed connects before the session counts as gone.
const GIVE_UP: u32 = 5;

#[derive(Serialize)]
struct Hello<'a> {
    after: u64,
    epoch: Option<&'a str>,
}

#[derive(Serialize)]
struct SendFrame<'a> {
    t: &'static str,
    cid: &'a str,
    text: &'a str,
    steer: bool,
}

type Acks = Mutex<HashMap<String, oneshot::Sender<Result<(), String>>>>;

struct Shared {
    writer: tokio::sync::Mutex<Option<OwnedWriteHalf>>,
    acks: Acks,
}

pub struct NativeEngine {
    shared: Arc<Shared>,
}

impl NativeEngine {
    async fn write(&self, line: String) -> Result<(), String> {
        let mut w = self.shared.writer.lock().await;
        let Some(w) = w.as_mut() else { return Err("not connected".into()) };
        w.write_all(format!("{line}\n").as_bytes()).await.map_err(|_| "not connected".to_string())
    }
}

impl Engine for NativeEngine {
    async fn send(&self, text: &str, steer: bool, cid: &str) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.shared.acks.lock().unwrap_or_else(|e| e.into_inner()).insert(cid.to_string(), tx);
        let line = serde_json::to_string(&SendFrame { t: "send", cid, text, steer }).map_err(|e| e.to_string())?;
        if let Err(e) = self.write(line).await {
            self.shared.acks.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
            return Err(e);
        }
        match tokio::time::timeout(ACK_WAIT, rx).await {
            // The supervisor's own API confirmed it: the hub need not wait for the record.
            Ok(Ok(Ok(()))) => Ok(true),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err("not connected".into()),
            Err(_) => {
                self.shared.acks.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
                Err("no answer from the session".into())
            }
        }
    }

    async fn stop(&self) -> Result<(), String> {
        self.write(r#"{"t":"stop"}"#.into()).await
    }
}

pub fn start(sock: PathBuf, log: Arc<LogBuffer>, name: String) -> (NativeEngine, mpsc::Receiver<EngineMsg>) {
    // Roomy: the hub reads this channel while it waits on an ack the reader
    // routes, and Codex streams deltas.
    let (tx, rx) = mpsc::channel(4096);
    let shared = Arc::new(Shared { writer: tokio::sync::Mutex::new(None), acks: Mutex::new(HashMap::new()) });
    tokio::spawn(run(sock, Arc::clone(&shared), tx, log, name));
    (NativeEngine { shared }, rx)
}

async fn run(sock: PathBuf, shared: Arc<Shared>, tx: mpsc::Sender<EngineMsg>, log: Arc<LogBuffer>, name: String) {
    let mut after = 0u64;
    let mut epoch: Option<String> = None;
    let mut failures = 0;
    loop {
        let stream = match UnixStream::connect(&sock).await {
            Ok(s) => s,
            Err(e) => {
                failures += 1;
                if failures >= GIVE_UP {
                    log.push(format!("chat feed gone {name}: {e}"));
                    let _ = tx.send(EngineMsg::Ended).await;
                    return;
                }
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        failures = 0;
        let (r, mut w) = stream.into_split();
        {
            // Stored before the first line goes out, so a send right after the
            // supervisor's hello finds the connection.
            let mut slot = shared.writer.lock().await;
            let hello = serde_json::to_string(&Hello { after, epoch: epoch.as_deref() }).unwrap_or_default();
            if w.write_all(format!("{hello}\n").as_bytes()).await.is_ok() {
                *slot = Some(w);
            }
        }
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut v) = serde_json::from_str::<Value>(&line) else { continue };
            let msg = match v["t"].as_str() {
                Some("hello") => {
                    let e = v["epoch"].as_str().map(str::to_string);
                    let replaced = epoch.is_some() && e != epoch;
                    epoch = e;
                    if !replaced {
                        continue;
                    }
                    // A new supervisor: its positions start again.
                    after = 0;
                    EngineMsg::Reset
                }
                Some("ev") => {
                    let seq = v["seq"].as_u64().unwrap_or(0);
                    if seq <= after {
                        continue;
                    }
                    after = seq;
                    EngineMsg::Ev(v["ev"].take())
                }
                Some("state") => EngineMsg::Status {
                    working: v["working"].as_bool().unwrap_or(false),
                    waiting: v["waiting"].as_bool().unwrap_or(false),
                },
                Some("ready") => EngineMsg::Ready,
                Some("delta") => EngineMsg::Delta {
                    id: v["id"].as_str().unwrap_or_default().to_string(),
                    text: v["text"].as_str().unwrap_or_default().to_string(),
                },
                Some("ack") => {
                    let cid = v["cid"].as_str().unwrap_or_default();
                    let waiter = shared.acks.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
                    if let Some(w) = waiter {
                        let result = if v["ok"] == true { Ok(()) } else { Err(v["error"].as_str().unwrap_or("not sent").to_string()) };
                        let _ = w.send(result);
                    }
                    continue;
                }
                _ => continue,
            };
            if tx.send(msg).await.is_err() {
                return; // the hub is gone
            }
        }
        *shared.writer.lock().await = None;
        // Waiters for acks on the dropped connection learn it at once.
        shared.acks.lock().unwrap_or_else(|e| e.into_inner()).clear();
        tokio::time::sleep(RETRY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::tmux_io::tests::tempfile_path;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    fn log() -> Arc<LogBuffer> {
        Arc::new(LogBuffer::new())
    }

    /// The supervisor side, driven line by line by the test.
    struct FakeSup {
        listener: UnixListener,
        conn: Option<(BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf)>,
    }
    impl FakeSup {
        async fn conn(&mut self) -> &mut (BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf) {
            if self.conn.is_none() {
                let (s, _) = tokio::time::timeout(std::time::Duration::from_secs(5), self.listener.accept()).await.expect("the engine connects").unwrap();
                let (r, w) = s.into_split();
                self.conn = Some((BufReader::new(r), w));
            }
            self.conn.as_mut().unwrap()
        }
        async fn read_line(&mut self) -> String {
            let (r, _) = self.conn().await;
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), r.read_line(&mut line)).await.expect("a line within 5 s").unwrap();
            line.trim_end().to_string()
        }
        async fn write_line(&mut self, line: &str) {
            let (_, w) = self.conn().await;
            w.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        }
        async fn drop_connection(&mut self) {
            self.conn = None;
        }
    }

    fn fake_supervisor(tag: &str) -> (PathBuf, FakeSup) {
        let dir = tempfile_path(&format!("native-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("chat.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        (sock, FakeSup { listener, conn: None })
    }

    #[tokio::test]
    async fn events_state_and_ready_flow_through_and_a_dropped_socket_resumes_without_repeats() {
        let (sock, mut sup) = fake_supervisor("flow");                      // accepts connections, exposes read_line()/write_line()
        let (_engine, mut rx) = start(sock, log(), "cdash-a".into());
        assert_eq!(sup.read_line().await, r#"{"after":0,"epoch":null}"#);
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":1}"#).await;
        sup.write_line(r#"{"t":"ev","seq":1,"ev":{"type":"result","subtype":"success"}}"#).await;
        sup.write_line(r#"{"t":"state","working":true,"waiting":false}"#).await;
        sup.write_line(r#"{"t":"ready"}"#).await;
        sup.write_line(r#"{"t":"delta","id":"m1","text":"Hi"}"#).await;
        assert!(matches!(rx.recv().await, Some(EngineMsg::Ev(_))));
        assert!(matches!(rx.recv().await, Some(EngineMsg::Status { working: true, waiting: false })));
        assert!(matches!(rx.recv().await, Some(EngineMsg::Ready)));
        assert!(matches!(rx.recv().await, Some(EngineMsg::Delta { .. })));
        sup.drop_connection().await;
        assert_eq!(sup.read_line().await, r#"{"after":1,"epoch":"e1"}"#, "resumes from where it was");
        sup.write_line(r#"{"t":"hello","epoch":"e2","head":0}"#).await;      // the supervisor was replaced
        assert!(matches!(rx.recv().await, Some(EngineMsg::Reset)));
    }
    #[tokio::test]
    async fn send_waits_for_the_ack_and_stop_is_fire_and_forget() {
        let (sock, mut sup) = fake_supervisor("send");
        let (engine, _rx) = start(sock, log(), "cdash-a".into());
        sup.read_line().await;
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
        let sending = tokio::spawn(async move { (engine.send("hi", true, "c1").await, engine) });
        assert_eq!(sup.read_line().await, r#"{"t":"send","cid":"c1","text":"hi","steer":true}"#);
        sup.write_line(r#"{"t":"ack","cid":"c1","ok":true}"#).await;
        let (sent, engine) = sending.await.unwrap();
        assert_eq!(sent, Ok(true), "the supervisor confirmed it");
        let failing = tokio::spawn(async move { (engine.send("no", false, "c2").await, engine) });
        sup.read_line().await;
        sup.write_line(r#"{"t":"ack","cid":"c2","ok":false,"error":"no turn"}"#).await;
        let (failed, engine) = failing.await.unwrap();
        assert_eq!(failed, Err("no turn".to_string()));
        engine.stop().await.unwrap();
        assert_eq!(sup.read_line().await, r#"{"t":"stop"}"#);
    }
    #[tokio::test(start_paused = true)]
    async fn a_supervisor_that_is_gone_ends_the_engine() {
        let sock = tempfile_path("gone").join("chat.sock");                  // nothing listens there
        let (_engine, mut rx) = start(sock, log(), "cdash-a".into());
        assert!(matches!(rx.recv().await, Some(EngineMsg::Ended)));
    }
}

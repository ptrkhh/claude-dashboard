//! One actor per chatted session: numbers events, fans them out, and owns the
//! held-message queue and the delivery rules.

pub use super::wire::{ClientFrame, HeldOp};
use super::queue::Queue;
use super::wire::sanitize;
use super::Frame;
use crate::host::log::LogBuffer;
use serde_json::{json, Value};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::Instant;

/// Events a fresh client gets before it has to page.
pub const BACKLOG: usize = 200;
pub const PAGE_MAX: usize = 200;
/// A sent message the session's record never showed is reported after this.
pub const CONFIRM_AFTER: Duration = Duration::from_secs(10);
/// How long after a Stop the hub looks for a prompt Claude handed back.
/// Transcript writes were seen up to 2.2 s late; an interrupt marker that
/// lands inside this window proves the prompt was taken, not handed back.
pub const STOP_SETTLE: Duration = Duration::from_millis(2500);
const TICK: Duration = Duration::from_millis(250);

/// What an engine reports about its session.
#[derive(Debug)]
pub enum EngineMsg {
    Ev(Value),
    Delta { id: String, text: String },
    Status { working: bool, waiting: bool },
    /// History has been replayed; subscribers may be answered.
    Ready,
    /// The conversation in the pane changed; positions restart at 1.
    Reset,
    /// The record's format is not understood; nothing is typed from now on.
    Unreadable,
    Ended,
}

/// The input side of a session.
pub trait Engine: Send + Sync + 'static {
    /// `Ok(true)` when the tool itself confirmed it accepted the message.
    fn send(&self, text: &str, steer: bool, cid: &str) -> impl Future<Output = Result<bool, String>> + Send;
    fn stop(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Empty an input box a stopped prompt was handed back into.
    fn clear_input(&self, _text: &str) -> impl Future<Output = ()> + Send {
        async {}
    }
}

pub struct Subscription {
    pub backlog: Vec<Frame>,
    pub live: broadcast::Receiver<Frame>,
}

enum Cmd {
    Subscribe { after: Option<u64>, epoch: Option<String>, reply: oneshot::Sender<Subscription> },
    Client { frame: ClientFrame, reply: oneshot::Sender<Option<Frame>> },
}

#[derive(Clone)]
pub struct Hub {
    cmds: mpsc::Sender<Cmd>,
}

impl Hub {
    pub fn spawn<E: Engine>(
        tool: &'static str,
        engine: E,
        events: mpsc::Receiver<EngineMsg>,
        log: Arc<LogBuffer>,
        name: String,
    ) -> Hub {
        let (cmds, rx) = mpsc::channel(64);
        let (out, _) = broadcast::channel(1024);
        let actor = Actor {
            tool,
            engine,
            log,
            name,
            out,
            epoch: new_epoch(),
            log_events: Vec::new(),
            pending: Vec::new(),
            queue: Queue::default(),
            working: false,
            waiting: false,
            ended: false,
            unreadable: false,
            ready: false,
            turn_ok: false,
            stopped_at: None,
            waiters: Vec::new(),
            last_state: String::new(),
        };
        tokio::spawn(actor.run(events, rx));
        Hub { cmds }
    }

    /// `None` once the hub has stopped.
    pub async fn subscribe(&self, after: Option<u64>, epoch: Option<String>) -> Option<Subscription> {
        let (reply, rx) = oneshot::channel();
        self.cmds.send(Cmd::Subscribe { after, epoch, reply }).await.ok()?;
        rx.await.ok()
    }

    /// The reply for `send` and `page`; `None` for the rest.
    pub async fn command(&self, frame: ClientFrame) -> Option<Frame> {
        let (reply, rx) = oneshot::channel();
        self.cmds.send(Cmd::Client { frame, reply }).await.ok()?;
        rx.await.ok().flatten()
    }

    pub fn is_closed(&self) -> bool {
        self.cmds.is_closed()
    }
}

fn new_epoch() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the OS CSPRNG must be available");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn frame(v: Value) -> Frame {
    Frame::from(v.to_string())
}

fn ack(cid: &str, status: &str, error: Option<&str>) -> Value {
    let mut a = json!({"t":"ack","cid":cid,"status":status});
    if let Some(e) = error {
        a["error"] = json!(e);
    }
    a
}

/// A prompt is a `user` event whose content is not a `tool_result` list.
fn prompt_text(ev: &Value) -> Option<String> {
    if ev["type"] != "user" {
        return None;
    }
    match &ev["message"]["content"] {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) if !blocks.iter().any(|b| b["type"] == "tool_result") => Some(
            blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""),
        ),
        _ => None,
    }
}

/// A message handed to the engine and not yet answered.
struct Pending {
    cid: String,
    text: String,
    at: Instant,
    /// The event that showed it, once one did.
    uuid: Option<String>,
    /// Already in the tool's hands: never times out, never blocks the queue.
    exempt: bool,
}

struct Actor<E> {
    tool: &'static str,
    engine: E,
    log: Arc<LogBuffer>,
    name: String,
    out: broadcast::Sender<Frame>,
    epoch: String,
    // ponytail: the whole conversation is kept in memory; page from the source by offset if sessions get large enough to matter.
    log_events: Vec<Value>,
    pending: Vec<Pending>,
    queue: Queue,
    working: bool,
    waiting: bool,
    ended: bool,
    unreadable: bool,
    ready: bool,
    /// The last turn ended on its own, so the next held message may go.
    turn_ok: bool,
    stopped_at: Option<Instant>,
    waiters: Vec<(Option<u64>, Option<String>, oneshot::Sender<Subscription>)>,
    last_state: String,
}

impl<E: Engine> Actor<E> {
    async fn run(mut self, mut events: mpsc::Receiver<EngineMsg>, mut cmds: mpsc::Receiver<Cmd>) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = events.recv() => match msg {
                    Some(m) => self.engine_msg(m).await,
                    None => {
                        self.end();
                        return;
                    }
                },
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Subscribe { after, epoch, reply }) => {
                        if self.ready {
                            let _ = reply.send(self.subscription(after, epoch));
                        } else {
                            self.waiters.push((after, epoch, reply));
                        }
                    }
                    Some(Cmd::Client { frame, reply }) => {
                        let r = self.client(frame).await;
                        let _ = reply.send(r);
                    }
                    // Every handle was dropped; nobody can reach this hub.
                    None => return,
                },
                _ = tick.tick() => self.tick().await,
            }
            self.release().await;
            self.publish_state();
        }
    }

    fn broadcast(&self, v: Value) {
        let _ = self.out.send(frame(v));
    }

    fn state(&self) -> Value {
        let held: Vec<Value> = self.queue.list().iter().map(|h| json!({"id":h.id,"text":h.text})).collect();
        json!({"t":"state","working":self.working,"waiting":self.waiting,"ended":self.ended,"unreadable":self.unreadable,"held":held})
    }

    fn publish_state(&mut self) {
        let s = self.state().to_string();
        if s != self.last_state {
            let _ = self.out.send(Frame::from(s.as_str()));
            self.last_state = s;
        }
    }

    fn ev_frame(&self, seq: usize) -> Value {
        json!({"t":"ev","seq":seq,"ev":self.log_events[seq - 1]})
    }

    /// Built inside the actor, so nothing is missed or repeated between the
    /// backlog and the live stream.
    fn subscription(&self, after: Option<u64>, epoch: Option<String>) -> Subscription {
        let live = self.out.subscribe();
        let head = self.log_events.len();
        let reset = epoch.as_deref() != Some(self.epoch.as_str());
        let first = match (reset, after) {
            (false, Some(a)) => (a as usize).min(head) + 1,
            _ => head.saturating_sub(BACKLOG) + 1,
        };
        let mut backlog = vec![frame(json!({
            "t":"hello","epoch":self.epoch,"tool":self.tool,"first":first,"head":head,"reset":reset
        }))];
        backlog.extend((first..=head).map(|seq| frame(self.ev_frame(seq))));
        backlog.push(frame(self.state()));
        Subscription { backlog, live }
    }

    fn append(&mut self, ev: Value) {
        self.log_events.push(ev);
        let seq = self.log_events.len();
        self.broadcast(self.ev_frame(seq));
    }

    async fn engine_msg(&mut self, m: EngineMsg) {
        match m {
            EngineMsg::Ev(ev) => self.event(ev),
            EngineMsg::Delta { id, text } => self.broadcast(json!({"t":"delta","id":id,"text":text})),
            EngineMsg::Status { working, waiting } => {
                self.working = working;
                self.waiting = waiting;
            }
            EngineMsg::Ready => {
                self.ready = true;
                for (after, epoch, reply) in std::mem::take(&mut self.waiters) {
                    let _ = reply.send(self.subscription(after, epoch));
                }
            }
            EngineMsg::Reset => {
                self.epoch = new_epoch();
                self.log_events.clear();
                self.pending.clear();
                self.broadcast(json!({"t":"reset","epoch":self.epoch}));
            }
            EngineMsg::Unreadable => self.unreadable = true,
            EngineMsg::Ended => self.end(),
        }
    }

    fn event(&mut self, mut ev: Value) {
        if let Some(text) = prompt_text(&ev) {
            if let Some(p) = self.pending.iter_mut().find(|p| p.uuid.is_none() && p.text == text) {
                ev["cid"] = json!(p.cid);
                p.uuid = Some(ev["uuid"].as_str().unwrap_or_default().to_string());
            }
            self.turn_ok = false;
        } else if ev["type"] == "assistant" || ev["type"] == "user" || ev["type"] == "result" {
            self.pending.retain(|p| p.uuid.is_none());
            if ev["type"] == "result" {
                if ev["subtype"] == "interrupted" {
                    self.queue.stop_all();
                } else {
                    self.turn_ok = true;
                }
            }
        } else if ev["type"] == "cdash" && ev["subtype"] == "queued" {
            // In the tool's own queue now: out of our hands, so it must not
            // time out or block the next held message.
            let text = ev["text"].as_str().unwrap_or_default();
            if let Some(p) = self.pending.iter_mut().find(|p| p.uuid.is_none() && !p.exempt && p.text == text) {
                p.exempt = true;
                ev["cid"] = json!(p.cid);
            }
        }
        self.append(ev);
    }

    async fn client(&mut self, f: ClientFrame) -> Option<Frame> {
        match f {
            ClientFrame::Send { cid, text } => Some(frame(self.send(cid, &text).await)),
            ClientFrame::Stop => {
                self.queue.stop_all();
                if self.working || !self.pending.is_empty() {
                    if let Err(e) = self.engine.stop().await {
                        self.log.push(format!("chat stop failed {}: {e}", self.name));
                    }
                    self.stopped_at = Some(Instant::now());
                }
                None
            }
            ClientFrame::Held { op: HeldOp::Remove, id } => {
                self.queue.take(&id);
                None
            }
            ClientFrame::Held { op: HeldOp::Now, id } => {
                if let Some(e) = self.refusal() {
                    // Kept in the queue: Enter into a dialog would answer it.
                    if let Some(h) = self.queue.list().iter().find(|h| h.id == id) {
                        self.broadcast(ack(&h.cid, "refused", Some(e)));
                    }
                } else if let Some(h) = self.queue.take(&id) {
                    let steer = self.working;
                    let a = self.deliver(&h.cid, &h.text, steer).await;
                    self.broadcast(a);
                }
                None
            }
            ClientFrame::Page { before, limit } => Some(frame(self.page(before, limit))),
        }
    }

    async fn send(&mut self, cid: String, text: &str) -> Value {
        let text = match sanitize(text) {
            Ok(t) => t,
            Err(e) => return ack(&cid, "refused", Some(e)),
        };
        if self.working && !self.ended && !self.unreadable && !self.waiting {
            let id = self.queue.hold(cid.clone(), text);
            let mut a = ack(&cid, "held", None);
            a["id"] = json!(id);
            return a;
        }
        if let Some(e) = self.refusal() {
            return ack(&cid, "refused", Some(e));
        }
        self.deliver(&cid, &text, false).await
    }

    /// Why nothing may be typed into the session right now, if anything.
    fn refusal(&self) -> Option<&'static str> {
        if self.ended {
            Some("session ended")
        } else if self.unreadable {
            Some("cannot read this session; use Terminal")
        } else if self.waiting {
            Some("answer in Terminal first")
        } else if self.stopped_at.is_some() && self.pending.iter().any(|p| !p.exempt) {
            // A paste now could join a prompt Claude is handing back.
            Some("stopping; send again in a moment")
        } else {
            None
        }
    }

    async fn deliver(&mut self, cid: &str, text: &str, steer: bool) -> Value {
        match self.engine.send(text, steer, cid).await {
            Err(e) => ack(cid, "refused", Some(&e)),
            Ok(confirmed) => {
                self.pending.push(Pending {
                    cid: cid.to_string(),
                    text: text.to_string(),
                    at: Instant::now(),
                    uuid: None,
                    exempt: confirmed,
                });
                self.log.push(format!("chat send {}", self.name));
                ack(cid, "sent", None)
            }
        }
    }

    async fn release(&mut self) {
        let idle = !self.working && !self.waiting && !self.unreadable && !self.ended;
        if !idle || !self.turn_ok || !self.pending.iter().all(|p| p.exempt) {
            return;
        }
        if let Some(h) = self.queue.next_auto() {
            self.turn_ok = false;
            let a = self.deliver(&h.cid, &h.text, false).await;
            self.broadcast(a);
        }
    }

    fn page(&self, before: u64, limit: Option<usize>) -> Value {
        let limit = limit.unwrap_or(PAGE_MAX).min(PAGE_MAX);
        let end = (before as usize).saturating_sub(1).min(self.log_events.len());
        let start = end.saturating_sub(limit);
        let events: Vec<Value> = (start + 1..=end).map(|seq| json!({"seq":seq,"ev":self.log_events[seq - 1]})).collect();
        json!({"t":"page","events":events,"more":start > 0})
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        let (late, keep): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| !p.exempt && p.uuid.is_none() && now.duration_since(p.at) > CONFIRM_AFTER);
        self.pending = keep;
        for p in late {
            self.broadcast(ack(&p.cid, "unconfirmed", None));
        }
        let Some(at) = self.stopped_at else { return };
        if now.duration_since(at) < STOP_SETTLE || self.working {
            if now.duration_since(at) > CONFIRM_AFTER {
                self.stopped_at = None;
            }
            return;
        }
        self.stopped_at = None;
        // Stopped before any reply: Claude put the prompt back in its input box.
        let Some(i) = self.pending.iter().rposition(|p| !p.exempt) else { return };
        let p = self.pending.remove(i);
        self.engine.clear_input(&p.text).await;
        if let Some(uuid) = &p.uuid {
            self.append(json!({"type":"cdash","subtype":"retract","uuid":uuid}));
        }
        self.broadcast(ack(&p.cid, "returned", None));
    }

    fn end(&mut self) {
        if self.ended {
            return;
        }
        self.ended = true;
        let held: Vec<String> = self.queue.clear().into_iter().map(|h| h.cid).collect();
        let pending: Vec<String> = self.pending.drain(..).map(|p| p.cid).collect();
        for cid in held.iter().chain(&pending) {
            self.broadcast(ack(cid, "refused", Some("session ended")));
        }
        self.publish_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone)]
    struct FakeEngine {
        calls: Arc<Mutex<Vec<String>>>,
        accepts: bool,
        slow: Option<Duration>,
    }
    impl Engine for FakeEngine {
        async fn send(&self, text: &str, steer: bool, _cid: &str) -> Result<bool, String> {
            match self.slow {
                Some(d) => {
                    self.calls.lock().unwrap().push(format!("begin:{text}"));
                    tokio::time::sleep(d).await;
                    self.calls.lock().unwrap().push(format!("end:{text}"));
                }
                None => self.calls.lock().unwrap().push(format!("send:{text}:{steer}")),
            }
            Ok(self.accepts)
        }
        async fn stop(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("stop".into());
            Ok(())
        }
        async fn clear_input(&self, text: &str) {
            self.calls.lock().unwrap().push(format!("clear:{text}"));
        }
    }

    type Calls = Arc<Mutex<Vec<String>>>;

    fn rig_with(slow: Option<Duration>) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        let calls = Calls::default();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let engine = FakeEngine { calls: calls.clone(), accepts: false, slow };
        let log = Arc::new(crate::host::log::LogBuffer::new());
        let hub = Hub::spawn("claude", engine, rx, log, "cdash-a".into());
        tx.try_send(EngineMsg::Ready).unwrap();
        (hub, tx, calls)
    }
    fn rig() -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        rig_with(None)
    }
    fn rig_slow(d: Duration) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        rig_with(Some(d))
    }
    fn frames(sub: &mut Subscription) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(f) = sub.live.try_recv() {
            out.push(serde_json::from_str(&f).unwrap());
        }
        out
    }
    fn prompt(uuid: &str, text: &str) -> Value {
        json!({"type":"user","uuid":uuid,"message":{"role":"user","content":text},"src":"typed"})
    }
    fn answer(uuid: &str) -> Value {
        json!({"type":"assistant","uuid":uuid,"message":{"id":"m","role":"assistant","content":[{"type":"text","text":"ok"}]}})
    }
    fn send(cid: &str, text: &str) -> ClientFrame {
        ClientFrame::Send { cid: cid.into(), text: text.into() }
    }
    fn ack_of(reply: Option<Frame>) -> Value {
        serde_json::from_str(&reply.expect("a reply")).unwrap()
    }
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    #[tokio::test]
    async fn a_fresh_subscriber_gets_hello_backlog_and_state_then_live_events() {
        let (hub, tx, _) = rig();
        for i in 0..3 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle().await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        let b: Vec<Value> = sub.backlog.iter().map(|f| serde_json::from_str(f).unwrap()).collect();
        assert_eq!((b[0]["t"].as_str(), b[0]["reset"].as_bool(), b[0]["first"].as_u64(), b[0]["head"].as_u64()), (Some("hello"), Some(true), Some(1), Some(3)));
        assert_eq!(b[1..4].iter().map(|f| f["seq"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(b[4]["t"], "state");
        tx.send(EngineMsg::Ev(answer("k"))).await.unwrap();
        settle().await;
        assert_eq!(frames(&mut sub)[0]["seq"], 4);
    }
    #[tokio::test]
    async fn a_known_position_resumes_and_a_stale_epoch_gets_a_reset_and_a_fresh_backlog() {
        let (hub, tx, _) = rig();
        for i in 0..5 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle().await;
        let first = hub.subscribe(None, None).await.unwrap();
        let epoch = serde_json::from_str::<Value>(&first.backlog[0]).unwrap()["epoch"].as_str().unwrap().to_string();
        let resumed = hub.subscribe(Some(3), Some(epoch)).await.unwrap();
        let r: Vec<Value> = resumed.backlog.iter().map(|f| serde_json::from_str(f).unwrap()).collect();
        assert_eq!(r[0]["reset"], false);
        assert_eq!(r.iter().filter_map(|f| f["seq"].as_u64()).collect::<Vec<_>>(), [4, 5]);
        let stale = hub.subscribe(Some(3), Some("an-older-agent".into())).await.unwrap();
        let s: Vec<Value> = stale.backlog.iter().map(|f| serde_json::from_str(f).unwrap()).collect();
        assert_eq!(s[0]["reset"], true);
        assert_eq!(s.iter().filter_map(|f| f["seq"].as_u64()).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    }
    #[tokio::test]
    async fn idle_sends_go_out_busy_sends_are_held_and_a_turn_that_ends_on_its_own_releases_one() {
        let (hub, tx, calls) = rig();
        assert_eq!(ack_of(hub.command(send("c1", "first")).await)["status"], "sent");
        tx.send(EngineMsg::Status { working: true, waiting: false }).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle().await;
        let held = ack_of(hub.command(send("c2", "second")).await);
        assert_eq!((held["status"].as_str(), held["id"].as_str()), (Some("held"), Some("h1")));
        hub.command(send("c3", "third")).await;
        tx.send(EngineMsg::Ev(answer("k1"))).await.unwrap();
        tx.send(EngineMsg::Ev(json!({"type":"result","subtype":"success"}))).await.unwrap();
        tx.send(EngineMsg::Status { working: false, waiting: false }).await.unwrap();
        settle().await;
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "send:second:false"], "one per turn");
    }
    #[tokio::test(start_paused = true)]
    async fn stop_keeps_held_messages_and_a_prompt_stopped_before_any_reply_is_handed_back() {
        let (hub, tx, calls) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        hub.command(send("c1", "first")).await;
        tx.send(EngineMsg::Status { working: true, waiting: false }).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle().await;
        hub.command(send("c2", "second")).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(EngineMsg::Status { working: false, waiting: false }).await.unwrap();
        tokio::time::sleep(STOP_SETTLE + Duration::from_millis(300)).await;
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "stop", "clear:first"]);
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["ev"]["subtype"] == "retract" && x["ev"]["uuid"] == "u1"));
        assert!(f.iter().any(|x| x["t"] == "ack" && x["cid"] == "c1" && x["status"] == "returned"));
        let state = f.iter().rev().find(|x| x["t"] == "state").unwrap();
        assert_eq!(state["held"][0]["text"], "second", "still listed, not sent");
        assert_eq!(ack_of(hub.command(send("c4", "typed while idle")).await)["status"], "sent");
        assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("send:second")));
    }
    #[tokio::test]
    async fn an_interrupt_from_any_door_stops_the_queue_and_send_now_steers() {
        let (hub, tx, calls) = rig();
        tx.send(EngineMsg::Status { working: true, waiting: false }).await.unwrap();
        settle().await;
        hub.command(send("c1", "held one")).await;
        hub.command(send("c2", "held two")).await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h2".into() }).await;
        tx.send(EngineMsg::Ev(json!({"type":"result","subtype":"interrupted"}))).await.unwrap();
        tx.send(EngineMsg::Status { working: false, waiting: false }).await.unwrap();
        settle().await;
        assert_eq!(*calls.lock().unwrap(), ["send:held two:true"], "steered at once; the other one stays held");
        hub.command(ClientFrame::Held { op: HeldOp::Remove, id: "h1".into() }).await;
        let sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(sub.backlog.last().unwrap()).unwrap()["held"], json!([]));
    }
    #[tokio::test(start_paused = true)]
    async fn two_identical_messages_are_confirmed_separately_in_order_and_silence_is_reported() {
        let (hub, tx, _) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        hub.command(send("c1", "same")).await;
        hub.command(send("c2", "same")).await;
        tx.send(EngineMsg::Ev(prompt("u1", "same"))).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u2", "same"))).await.unwrap();
        settle().await;
        let cids: Vec<String> = frames(&mut sub).iter().filter(|f| f["ev"]["type"] == "user").map(|f| f["ev"]["cid"].as_str().unwrap().to_string()).collect();
        assert_eq!(cids, ["c1", "c2"]);
        hub.command(send("c3", "never shows up")).await;
        tokio::time::sleep(CONFIRM_AFTER + Duration::from_millis(500)).await;
        assert!(frames(&mut sub).iter().any(|f| f["t"] == "ack" && f["cid"] == "c3" && f["status"] == "unconfirmed"));
    }
    #[tokio::test]
    async fn waiting_and_ended_sessions_refuse_sends_and_ended_reports_held_messages_as_not_sent() {
        let (hub, tx, calls) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(EngineMsg::Status { working: true, waiting: false }).await.unwrap();
        settle().await;
        hub.command(send("c1", "held")).await;
        tx.send(EngineMsg::Status { working: false, waiting: true }).await.unwrap();
        settle().await;
        assert_eq!(ack_of(hub.command(send("c2", "x")).await)["status"], "refused");
        assert!(calls.lock().unwrap().is_empty(), "nothing is typed into a dialog");
        tx.send(EngineMsg::Ended).await.unwrap();
        settle().await;
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["t"] == "ack" && x["cid"] == "c1" && x["status"] == "refused" && x["error"] == "session ended"));
        assert!(f.iter().any(|x| x["t"] == "state" && x["ended"] == true && x["held"] == json!([])));
        assert_eq!(ack_of(hub.command(send("c3", "x")).await)["status"], "refused");
    }
    #[tokio::test]
    async fn sends_from_two_clients_reach_the_engine_one_at_a_time() {
        let (hub, _tx, calls) = rig_slow(Duration::from_millis(50));   // FakeEngine::send sleeps, and records "begin:"/"end:"
        let (a, b) = (hub.clone(), hub.clone());
        tokio::join!(a.command(send("c1", "one")), b.command(send("c2", "two")));
        let c = calls.lock().unwrap().clone();
        assert!(c == ["begin:one", "end:one", "begin:two", "end:two"] || c == ["begin:two", "end:two", "begin:one", "end:one"]);
    }
    #[tokio::test]
    async fn paging_returns_older_events_and_reset_starts_a_new_conversation() {
        let (hub, tx, _) = rig();
        for i in 0..(BACKLOG + 50) { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle().await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        let hello: Value = serde_json::from_str(&sub.backlog[0]).unwrap();
        assert_eq!(hello["first"], 51);
        let page: Value = serde_json::from_str(&hub.command(ClientFrame::Page { before: 51, limit: Some(30) }).await.unwrap()).unwrap();
        assert_eq!((page["events"].as_array().unwrap().len(), page["events"][0]["seq"].as_u64(), page["more"].as_bool()), (30, Some(21), Some(true)));
        tx.send(EngineMsg::Reset).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("n1", "new"))).await.unwrap();
        settle().await;
        let f = frames(&mut sub);
        assert_eq!(f[0]["t"], "reset");
        assert_eq!(f[1]["seq"], 1);
    }
    #[tokio::test]
    async fn an_unreadable_session_says_so_and_types_nothing() {
        let (hub, tx, calls) = rig();
        tx.send(EngineMsg::Unreadable).await.unwrap();
        settle().await;
        let sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(sub.backlog.last().unwrap()).unwrap()["unreadable"], true);
        let ack = ack_of(hub.command(send("c1", "x")).await);
        assert_eq!((ack["status"].as_str(), ack["error"].as_str()), (Some("refused"), Some("cannot read this session; use Terminal")));
        assert!(calls.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn send_now_never_types_into_a_dialog_or_an_unreadable_session() {
        let (hub, tx, calls) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(EngineMsg::Status { working: true, waiting: false }).await.unwrap();
        settle().await;
        hub.command(send("c1", "held")).await;
        tx.send(EngineMsg::Status { working: false, waiting: true }).await.unwrap();
        settle().await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
        tx.send(EngineMsg::Unreadable).await.unwrap();
        settle().await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
        assert!(calls.lock().unwrap().is_empty(), "Enter in a permission dialog would approve it");
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["t"] == "ack" && x["cid"] == "c1" && x["status"] == "refused" && x["error"] == "answer in Terminal first"));
        let state = f.iter().rev().find(|x| x["t"] == "state").unwrap();
        assert_eq!(state["held"][0]["id"], "h1", "still held, not lost");
    }
    #[tokio::test]
    async fn a_message_claude_queued_carries_its_client_id() {
        let (hub, tx, _) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(EngineMsg::Status { working: true, waiting: false }).await.unwrap();
        settle().await;
        hub.command(send("c1", "later")).await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
        tx.send(EngineMsg::Ev(json!({"type":"cdash","subtype":"queued","text":"later"}))).await.unwrap();
        settle().await;
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["ev"]["subtype"] == "queued" && x["ev"]["cid"] == "c1"));
    }
    #[tokio::test(start_paused = true)]
    async fn nothing_is_pasted_while_a_stopped_prompt_may_still_be_handed_back() {
        let (hub, tx, calls) = rig();
        hub.command(send("c1", "first")).await;
        tx.send(EngineMsg::Status { working: true, waiting: false }).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle().await;
        hub.command(ClientFrame::Stop).await;
        tx.send(EngineMsg::Status { working: false, waiting: false }).await.unwrap();
        settle().await;
        let early = ack_of(hub.command(send("c2", "second")).await);
        assert_eq!((early["status"].as_str(), early["error"].as_str()), (Some("refused"), Some("stopping; send again in a moment")));
        assert!(STOP_SETTLE >= Duration::from_millis(2500), "transcript writes were seen up to 2.2 s late");
        tokio::time::sleep(STOP_SETTLE + Duration::from_millis(300)).await;
        assert_eq!(ack_of(hub.command(send("c3", "third")).await)["status"], "sent");
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "stop", "clear:first", "send:third:false"]);
    }

}

//! The Claude engine: tails the session transcript and the session state
//! file, and types through tmux.

use super::hub::{Engine, EngineMsg};
use super::tap::Tap;
use super::tmux_io;
use crate::collect::ctx::Ctx;
use crate::collect::lookup::session_file_for;
use crate::host::cmd::Runner;
use crate::host::log::LogBuffer;
use crate::parse::paths::project_dir_name;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;

/// Most bytes one poll reads, so a multi-megabyte record cannot stall a tick.
const READ_MAX: u64 = 4 * 1024 * 1024;
const TICK: Duration = Duration::from_millis(150);
/// `has-session` every 13th tick, about every two seconds.
const ALIVE_EVERY: u64 = 13;

/// Complete lines appended to a file since the last poll.
pub struct Tail {
    path: PathBuf,
    offset: u64,
    carry: Vec<u8>,
}

impl Tail {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, carry: Vec::new() }
    }

    pub async fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let len = match tokio::fs::metadata(&self.path).await {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if len < self.offset {
            self.offset = 0;
            self.carry.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        let mut f = tokio::fs::File::open(&self.path).await?;
        f.seek(std::io::SeekFrom::Start(self.offset)).await?;
        let mut buf = Vec::new();
        f.take(READ_MAX.min(len - self.offset)).read_to_end(&mut buf).await?;
        self.offset += buf.len() as u64;
        self.carry.extend_from_slice(&buf);
        let Some(last) = self.carry.iter().rposition(|&b| b == b'\n') else { return Ok(Vec::new()) };
        let rest = self.carry.split_off(last + 1);
        let done = std::mem::replace(&mut self.carry, rest);
        Ok(done[..last].split(|&b| b == b'\n').map(|l| String::from_utf8_lossy(l).into_owned()).collect())
    }
}

pub struct ClaudeEngine {
    runner: Arc<Runner>,
    log: Arc<LogBuffer>,
    name: String,
}

impl Engine for ClaudeEngine {
    async fn send(&self, text: &str, _steer: bool, _cid: &str) -> Result<bool, String> {
        // A paste cannot be confirmed by the TUI; the transcript confirms it.
        tmux_io::paste(&self.runner, &self.name, text).await.map(|()| false)
    }

    async fn stop(&self) -> Result<(), String> {
        tmux_io::escape(&self.runner, &self.name).await
    }

    async fn clear_input(&self, text: &str) {
        if let Err(e) = tmux_io::clear_input(&self.runner, &self.name, text).await {
            self.log.push(format!("chat clear failed {}: {e}", self.name));
        }
    }
}

pub fn start(ctx: Arc<Ctx>, name: String, pid: i32) -> (ClaudeEngine, mpsc::Receiver<EngineMsg>) {
    let (tx, rx) = mpsc::channel(256);
    let engine = ClaudeEngine { runner: Arc::clone(&ctx.runner), log: Arc::clone(&ctx.host.log), name: name.clone() };
    tokio::spawn(run(ctx, name, pid, tx));
    (engine, rx)
}

/// The conversation currently followed.
struct Conv {
    sid: String,
    tail: Tail,
    tap: Tap,
    ready: bool,
}

// ponytail: polls for the pane's whole life once a chat was opened; park the loop when no client is attached if it ever shows in a profile.
async fn run(ctx: Arc<Ctx>, name: String, pid: i32, tx: mpsc::Sender<EngineMsg>) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut conv: Option<Conv> = None;
    let mut last: Option<(bool, bool)> = None;
    let mut blind_sent = false;
    let target = format!("={name}");
    for n in 1u64.. {
        tick.tick().await;
        let file = session_file_for(&ctx.claude_dir, pid).await;
        let status = file.as_ref().and_then(|f| f.status.clone());
        if let Some((sid, cwd)) = file.as_ref().and_then(|f| Some((f.session_id.clone()?, f.cwd.clone()?))) {
            if conv.as_ref().is_none_or(|c| c.sid != sid) {
                // `/clear` or `/resume` gave the pane a new conversation.
                if conv.is_some() && tx.send(EngineMsg::Reset).await.is_err() {
                    return;
                }
                let path = ctx.claude_dir.join("projects").join(project_dir_name(&cwd)).join(format!("{sid}.jsonl"));
                conv = Some(Conv { sid, tail: Tail::new(path), tap: Tap::default(), ready: false });
                last = None;
            }
        }
        if let Some(c) = conv.as_mut() {
            let lines = match c.tail.poll().await {
                Ok(l) => l,
                Err(e) => {
                    ctx.host.log.push(format!("chat read failed {name}: {e}"));
                    Vec::new()
                }
            };
            let idle_read = lines.is_empty();
            for line in lines {
                for ev in c.tap.feed(&line) {
                    if tx.send(EngineMsg::Ev(ev)).await.is_err() {
                        return;
                    }
                }
            }
            if idle_read && !c.ready {
                c.ready = true;
                if tx.send(EngineMsg::Ready).await.is_err() {
                    return;
                }
            }
            if c.ready {
                let working = match status.as_deref() {
                    Some(s) => s == "busy" || s == "shell",
                    None => c.tap.turn_open(),
                };
                let now = (working, status.as_deref() == Some("waiting"));
                if last != Some(now) {
                    last = Some(now);
                    if tx.send(EngineMsg::Status { working: now.0, waiting: now.1 }).await.is_err() {
                        return;
                    }
                }
            }
            if c.tap.blind() && !blind_sent {
                blind_sent = true;
                if tx.send(EngineMsg::Unreadable).await.is_err() {
                    return;
                }
            }
        }
        if n % ALIVE_EVERY == 0
            && ctx.runner.run_checked("tmux", &["has-session", "-t", &target], "tmux has-session").await.is_err()
        {
            let _ = tx.send(EngineMsg::Ended).await;
            return;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::chat::tmux_io::tests::{stub_tmux, tempfile_path};
    use crate::host::cmd::Runner;
    use crate::host::log::LogBuffer;
    use std::path::Path;
    use std::time::Duration;

    pub(crate) fn append(f: &Path, s: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(f).unwrap().write_all(s.as_bytes()).unwrap();
    }

    /// Lines `r.start` to `r.end - 1` of the fixture, counting from 1.
    pub(crate) fn fixture_lines(r: std::ops::Range<usize>) -> String {
        let all: Vec<&str> = include_str!("testdata/claude.jsonl").lines().collect();
        all[r.start - 1..r.end - 1].iter().map(|l| format!("{l}\n")).collect()
    }

    pub(crate) fn write_session(dir: &Path, json: &str) {
        let tmp = dir.join("sessions/.4242.json.tmp");
        std::fs::write(&tmp, json).unwrap();
        std::fs::rename(tmp, dir.join("sessions/4242.json")).unwrap();
    }

    pub(crate) fn write_transcript(dir: &Path, sid: &str, lines: &str) {
        let proj = dir.join("projects/-proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{sid}.jsonl")), lines).unwrap();
    }

    pub(crate) fn make_tmux_fail(dir: &Path) {
        stub_tmux(dir, "", 1);
    }

    pub(crate) fn ctx_with_path(dir: PathBuf, path: String) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let host = crate::host::init::Host {
            runner: Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, dir, None))
    }

    /// A claude_dir with `sessions/4242.json` for `sid-1` in `/proj` and its
    /// transcript, and a stub tmux on PATH that answers `answer` and exits 0.
    /// `"fallback"` writes no `status` and only lines 1 to 7 (a turn still open).
    pub(crate) fn fixture_with(tag: &str, answer: &str) -> (Arc<Ctx>, PathBuf) {
        let dir = tempfile_path(tag);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(dir.join("history.jsonl"), "").unwrap();
        if tag == "fallback" {
            write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj"}"#);
            write_transcript(&dir, "sid-1", &fixture_lines(1..8));
        } else {
            write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#);
            write_transcript(&dir, "sid-1", &fixture_lines(1..10));
        }
        let path = stub_tmux(&dir, answer, 0);
        (ctx_with_path(dir.clone(), path), dir)
    }

    fn fixture_ctx(tag: &str) -> (Arc<Ctx>, PathBuf) {
        fixture_with(tag, "")
    }

    async fn next_msg(rx: &mut mpsc::Receiver<EngineMsg>) -> EngineMsg {
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("no engine message within 5 s").expect("engine ended")
    }

    async fn collect_until_ready(rx: &mut mpsc::Receiver<EngineMsg>) -> Vec<EngineMsg> {
        let mut got = Vec::new();
        loop {
            let m = next_msg(rx).await;
            let ready = matches!(m, EngineMsg::Ready);
            got.push(m);
            if ready {
                return got;
            }
        }
    }

    async fn next_of(rx: &mut mpsc::Receiver<EngineMsg>, f: impl Fn(&EngineMsg) -> bool) -> EngineMsg {
        loop {
            let m = next_msg(rx).await;
            if f(&m) {
                return m;
            }
        }
    }

    async fn next_status(rx: &mut mpsc::Receiver<EngineMsg>) -> EngineMsg {
        next_of(rx, |m| matches!(m, EngineMsg::Status { .. })).await
    }

    #[tokio::test]
    async fn a_line_split_across_two_writes_is_delivered_once_whole() {
        let f = tempfile_path("tail");
        std::fs::write(&f, "{\"a\":1}\n{\"b\":").unwrap();
        let mut t = Tail::new(f.clone());
        assert_eq!(t.poll().await.unwrap(), ["{\"a\":1}"]);
        assert!(t.poll().await.unwrap().is_empty());
        append(&f, "2}\n");
        assert_eq!(t.poll().await.unwrap(), ["{\"b\":2}"]);
    }
    #[tokio::test]
    async fn a_huge_line_and_a_missing_or_shrunken_file_do_not_break_the_tail() {
        let f = tempfile_path("tail-big");
        let mut t = Tail::new(f.clone());
        assert!(t.poll().await.unwrap().is_empty(), "not there yet is not an error");
        std::fs::write(&f, format!("{}\nsmall\n", "x".repeat(9 * 1024 * 1024))).unwrap();
        let mut got = Vec::new();
        for _ in 0..8 { got.extend(t.poll().await.unwrap()); }
        assert_eq!((got.len(), got[1].as_str()), (2, "small"));
        std::fs::write(&f, "fresh\n").unwrap();
        assert_eq!(t.poll().await.unwrap(), ["fresh"], "a file that got shorter is read from the start");
    }
    #[tokio::test]
    async fn the_engine_replays_the_transcript_reports_state_and_follows_a_new_session_id() {
        // claude_dir with sessions/4242.json {"sessionId":"sid-1","cwd":"/proj","status":"busy"}
        // and projects/-proj/sid-1.jsonl holding fixture lines 1 to 9; PATH = stub tmux that exits 0.
        // fixture_lines(a..b) returns lines a to b-1 of testdata/claude.jsonl, counting from 1.
        let (ctx, dir) = fixture_ctx("engine");
        let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        let got = collect_until_ready(&mut rx).await;              // everything up to and including Ready
        assert_eq!(got.iter().filter(|m| matches!(m, EngineMsg::Ev(_))).count(), 5);
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: true, waiting: false }));
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"dialog open"}"#);
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: false, waiting: true }));
        write_transcript(&dir, "sid-2", &fixture_lines(2..3));
        write_session(&dir, r#"{"sessionId":"sid-2","cwd":"/proj","status":"idle"}"#);
        assert!(matches!(next_of(&mut rx, |m| matches!(m, EngineMsg::Reset)).await, EngineMsg::Reset));
        assert!(matches!(next_of(&mut rx, |m| matches!(m, EngineMsg::Ev(_))).await, EngineMsg::Ev(_)));
    }
    #[tokio::test]
    async fn without_a_status_field_the_transcript_decides_and_a_dead_pane_ends_the_engine() {
        // sessions/4242.json without "status"; transcript = fixture lines 1 to 7 (a prompt and a reply, no turn end yet).
        let (ctx, dir) = fixture_ctx("fallback");
        let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: true, .. }));
        make_tmux_fail(&dir);                                      // rewrite the stub so has-session exits 1
        assert!(matches!(next_of(&mut rx, |m| matches!(m, EngineMsg::Ended)).await, EngineMsg::Ended));
    }
}

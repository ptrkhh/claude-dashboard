//! A live terminal on a dashboard session: `tmux attach` in a PTY, bridged to
//! a WebSocket. This half is the request validation; the bridge follows.

use super::ctx::Ctx;
use super::validate::BadRequest;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use serde::Deserialize;
use std::io;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;

/// Upper bound for either dimension. Far past any real screen, low enough
/// that a hostile value cannot ask tmux for an absurd allocation.
const MAX_DIM: u16 = 1000;

/// Whether a WebSocket upgrade carrying `origin` may attach.
///
/// A WebSocket upgrade is not subject to CORS, so without this any page open
/// on the phone could reach `ws://localhost:<port>` and type into a session,
/// which amounts to a shell. Only host[:port] is compared, never the scheme:
/// behind a TLS-terminating proxy the agent sees plain HTTP while the browser
/// sends `https://`, and a page must already be served from the agent's own
/// host to pass either way. `public_url` (`CDASH_PUBLIC_URL`) covers a proxy
/// that rewrites `Host`. Does not stop DNS rebinding; see the spec.
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>, public_url: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    if origin == "http://tauri.localhost" || origin == "tauri://localhost" {
        return true;
    }
    let Some(o) = authority(origin) else {
        return false;
    };
    host.is_some_and(|h| h.eq_ignore_ascii_case(o))
        || public_url
            .and_then(authority)
            .is_some_and(|p| p.eq_ignore_ascii_case(o))
}

/// `host[:port]` of an absolute URL: what follows `://`, up to the first `/`.
fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let a = rest.split('/').next().unwrap_or_default();
    (!a.is_empty()).then_some(a)
}

fn dim(v: Option<&str>) -> Option<u16> {
    v?.parse::<u16>().ok().filter(|n| (1..=MAX_DIM).contains(n))
}

/// The initial `cols`/`rows` query values. Required, so the first frame is
/// already drawn at phone width rather than reflowed from 80x24.
pub fn parse_size(cols: Option<&str>, rows: Option<&str>) -> Result<(u16, u16), BadRequest> {
    match (dim(cols), dim(rows)) {
        (Some(c), Some(r)) => Ok((c, r)),
        _ => Err(BadRequest(format!(
            "bad size: cols and rows must be 1..={MAX_DIM}"
        ))),
    }
}

#[derive(Deserialize)]
struct Resize {
    resize: [u16; 2],
}

/// `{"resize":[cols,rows]}`, the only text frame the browser sends. Anything
/// else is `None` and the bridge ignores it.
pub fn parse_resize(text: &str) -> Option<(u16, u16)> {
    let [c, r] = serde_json::from_str::<Resize>(text).ok()?.resize;
    let ok = |n: u16| (1..=MAX_DIM).contains(&n);
    (ok(c) && ok(r)).then_some((c, r))
}

/// A WebSocket close reason may carry at most 123 bytes.
fn close_reason(s: &str) -> String {
    let mut end = s.len().min(123);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn set_size(fd: impl AsFd, cols: u16, rows: u16) -> io::Result<()> {
    let ws = rustix::termios::Winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    Ok(rustix::termios::tcsetwinsize(fd, ws)?)
}

/// A PTY pair. The master is non-blocking (for `AsyncFd`); `openpt` has no
/// flag for that, so it is set afterwards. The size is set before anything
/// attaches, so tmux draws its first frame at the phone's width.
fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let path = ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        path.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    set_size(&master, cols, rows)?;
    rustix::fs::fcntl_setfl(&master, OFlags::NONBLOCK)?;
    Ok((master, slave))
}

/// `tmux attach` as the bridge runs it. `-u` because the agent's environment
/// may carry no UTF-8 locale, and the tmux client would then draw every box
/// character as `_`. `TMUX` stays: it names the server `new-session` used, and
/// tmux refuses to nest only when the client's tty is one of its pane ttys,
/// which this fresh PTY never is. `=name` is an exact match, where a bare name
/// would also match a longer session it prefixes.
#[allow(clippy::disallowed_types)] // a PTY child, which Runner (piped output, timeout) cannot express
fn attach_command(name: &str, path: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("tmux");
    cmd.args(["-u", "attach-session", "-t", &format!("={name}")])
        .env("PATH", path)
        .env("TERM", "xterm-256color")
        .kill_on_drop(true);
    cmd
}

fn spawn_attach(name: &str, path: &str, slave: OwnedFd) -> io::Result<tokio::process::Child> {
    let mut cmd = attach_command(name, path);
    cmd.stdin(std::process::Stdio::from(slave.try_clone()?))
        .stdout(std::process::Stdio::from(slave.try_clone()?))
        .stderr(std::process::Stdio::from(slave));
    // SAFETY: runs in the forked child before exec, after stdio is
    // redirected. setsid and TIOCSCTTY are single syscalls with no allocation,
    // which is all a post-fork closure may do. Together they make the PTY
    // (now fd 0) the child's controlling terminal, so tmux gets SIGWINCH.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            Ok(())
        });
    }
    cmd.spawn()
    // `cmd` drops here, closing the parent's copies of the slave, so the
    // master reads EIO once tmux exits.
}

/// Write every byte, however the non-blocking fd splits it.
async fn write_all(fd: &AsyncFd<OwnedFd>, mut b: &[u8]) -> io::Result<()> {
    while !b.is_empty() {
        let mut g = fd.writable().await?;
        match g.try_io(|f| Ok(rustix::io::write(f.get_ref(), b)?)) {
            Ok(Ok(n)) => b = &b[n..],
            Ok(Err(e)) => return Err(e),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

/// How often the bridge pings the browser. A phone that loses its network
/// sends no FIN, so without this `recv` waits forever and the `tmux attach`
/// child leaks; a peer that answers nothing for one full interval is gone.
/// Short under test so a silent client is caught within the test's patience.
const PING_EVERY: std::time::Duration =
    std::time::Duration::from_secs(if cfg!(test) { 2 } else { 30 });

enum Ended {
    /// The browser went away; nothing left to tell it.
    Client,
    /// `tmux attach` exited: the session died or the user detached.
    Tmux,
}

async fn pump(
    ctx: &Ctx,
    name: &str,
    cols: u16,
    rows: u16,
    ws: &mut WebSocket,
) -> io::Result<Ended> {
    let (master, slave) = open_pty(cols, rows)?;
    let _child = spawn_attach(name, &ctx.host.path, slave)?; // killed on drop
    let master = AsyncFd::new(master)?;
    let mut buf = vec![0u8; 16 * 1024];
    let mut ping = tokio::time::interval(PING_EVERY);
    // Burst (the default) would fire missed ticks back to back after a stall
    // (a slow send on a bad mobile link), and the second would see `heard`
    // still false and cut off a live client.
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heard = true;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if !std::mem::take(&mut heard) {
                    return Ok(Ended::Client);
                }
                if ws.send(Message::Ping(axum::body::Bytes::new())).await.is_err() {
                    return Ok(Ended::Client);
                }
            }
            r = master.readable() => {
                let mut g = r?;
                match g.try_io(|f| Ok(rustix::io::read(f.get_ref(), &mut buf[..])?)) {
                    Ok(Ok(0)) => return Ok(Ended::Tmux),
                    Ok(Ok(n)) => {
                        let frame = Message::Binary(axum::body::Bytes::copy_from_slice(&buf[..n]));
                        if ws.send(frame).await.is_err() {
                            return Ok(Ended::Client);
                        }
                    }
                    // Linux reports a PTY whose last slave closed as EIO.
                    Ok(Err(e)) if e.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
                        return Ok(Ended::Tmux)
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(_would_block) => {}
                }
            }
            msg = ws.recv() => {
                heard = true;
                match msg {
                    Some(Ok(Message::Binary(b))) => write_all(&master, &b).await?,
                    Some(Ok(Message::Text(t))) => {
                        if let Some((c, r)) = parse_resize(t.as_str()) {
                            set_size(master.get_ref(), c, r)?;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Ok(Ended::Client),
                    Some(Ok(_)) => {} // ping/pong: axum answers pings itself
                }
            },
        }
    }
}

/// Run one terminal connection to the end, then tell the browser why it ended.
pub async fn bridge(ctx: Arc<Ctx>, name: String, cols: u16, rows: u16, mut ws: WebSocket) {
    ctx.host.log.push(format!("term open {name}"));
    let close = match pump(&ctx, &name, cols, rows, &mut ws).await {
        Ok(Ended::Client) => None,
        Ok(Ended::Tmux) => {
            let target = format!("={name}");
            let alive = ctx
                .runner
                .run_checked("tmux", &["has-session", "-t", &target], "tmux has-session")
                .await
                .is_ok();
            Some(if alive {
                (4001, "detached".to_string())
            } else {
                (4000, "session ended".to_string())
            })
        }
        Err(e) => {
            ctx.host.log.push(format!("term failed {name}: {e}"));
            Some((1011, close_reason(&e.to_string())))
        }
    };
    if let Some((code, reason)) = close {
        let _ = ws
            .send(Message::Close(Some(CloseFrame {
                code,
                reason: reason.into(),
            })))
            .await;
        // Wait for the browser's Close reply. Dropping the socket with a frame
        // still unread (a pong, a keystroke) makes the kernel answer RST, and
        // the browser can then lose the close code and see a plain 1006.
        let drain = async { while let Some(Ok(_)) = ws.recv().await {} };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), drain).await;
    }
    ctx.host.log.push(format!("term close {name}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_allows_what_only_the_dashboard_itself_can_send() {
        // Non-browser clients send no Origin and are not a cross-site vector.
        assert!(origin_allowed(None, Some("localhost:23274"), None));
        assert!(origin_allowed(
            Some("http://localhost:23274"),
            Some("localhost:23274"),
            None
        ));
        assert!(origin_allowed(
            Some("http://LOCALHOST:23274"),
            Some("localhost:23274"),
            None
        ));
        // Behind a TLS-terminating proxy the agent sees plain HTTP while the
        // browser sends https; the scheme must not matter.
        assert!(origin_allowed(
            Some("https://dash.example.com"),
            Some("dash.example.com"),
            None
        ));
        // A proxy that rewrites Host is covered by CDASH_PUBLIC_URL.
        assert!(origin_allowed(
            Some("https://dash.example.com"),
            Some("127.0.0.1:23274"),
            Some("https://dash.example.com/"),
        ));
        assert!(origin_allowed(
            Some("http://tauri.localhost"),
            Some("localhost:23274"),
            None
        ));
        assert!(origin_allowed(
            Some("tauri://localhost"),
            Some("localhost:23274"),
            None
        ));
    }

    #[test]
    fn origin_refuses_every_other_page() {
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("localhost:23274"),
            None
        ));
        // Same host, different port: a different origin, possibly another app.
        assert!(!origin_allowed(
            Some("http://localhost:3000"),
            Some("localhost:23274"),
            None
        ));
        // Sandboxed iframes and file:// pages send the literal "null".
        assert!(!origin_allowed(Some("null"), Some("localhost:23274"), None));
        assert!(!origin_allowed(Some("http://localhost:23274"), None, None));
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("127.0.0.1:23274"),
            Some("https://dash.example.com"),
        ));
        // Lookalikes of the Tauri origins are not Tauri.
        assert!(!origin_allowed(
            Some("http://tauri.localhost.evil.example"),
            Some("x"),
            None
        ));
    }

    #[test]
    fn size_must_be_two_integers_in_range() {
        assert_eq!(parse_size(Some("80"), Some("24")).unwrap(), (80, 24));
        assert_eq!(parse_size(Some("1"), Some("1000")).unwrap(), (1, 1000));
        for (c, r) in [
            (None, Some("24")),
            (Some("80"), None),
            (Some("0"), Some("24")),
            (Some("80"), Some("1001")),
            (Some("-1"), Some("24")),
            (Some("eighty"), Some("24")),
        ] {
            let e = parse_size(c, r).unwrap_err();
            assert!(e.0.contains("bad size"), "{c:?}x{r:?} gave {}", e.0);
        }
    }

    #[test]
    fn resize_message_is_the_one_text_frame_the_bridge_reads() {
        assert_eq!(parse_resize(r#"{"resize":[100,30]}"#), Some((100, 30)));
        assert_eq!(parse_resize(r#"{"resize":[0,30]}"#), None);
        assert_eq!(parse_resize(r#"{"resize":[100,5000]}"#), None);
        assert_eq!(parse_resize(r#"{"resize":[100]}"#), None);
        assert_eq!(parse_resize("hello"), None);
    }

    use crate::http::serve::{serve, tests::cfg_for};
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message as WsMsg};

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    fn have_tmux() -> bool {
        #[allow(clippy::disallowed_types)] // test-only probe, no Runner in scope
        let ok = std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok();
        if !ok {
            eprintln!("tmux not installed; skipping");
        }
        ok
    }

    fn tmux(args: &[&str]) -> std::process::Output {
        #[allow(clippy::disallowed_types)] // test-only fixture control
        std::process::Command::new("tmux")
            .args(args)
            .output()
            .unwrap()
    }

    /// A detached `sh` session, killed on drop.
    struct Session(String);
    impl Session {
        fn new(tag: &str) -> Self {
            let name = format!("cdash-termtest-{}-{tag}", std::process::id());
            let _ = tmux(&["kill-session", "-t", &format!("={name}")]);
            assert!(tmux(&[
                "new-session",
                "-d",
                "-s",
                &name,
                "-x",
                "80",
                "-y",
                "24",
                "sh"
            ])
            .status
            .success());
            Self(name)
        }
        fn clients(&self) -> String {
            let o = tmux(&[
                "list-clients",
                "-t",
                &format!("={}", self.0),
                "-F",
                "#{client_width}x#{client_height}",
            ]);
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = tmux(&["kill-session", "-t", &format!("={}", self.0)]);
        }
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-term-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    async fn connect(
        addr: std::net::SocketAddr,
        query: &str,
        origin: Option<&str>,
    ) -> Result<Ws, tungstenite::Error> {
        let mut req = format!("ws://{addr}/api/term?{query}")
            .into_client_request()
            .unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        tokio_tungstenite::connect_async(req)
            .await
            .map(|(ws, _)| ws)
    }

    fn status_of(r: Result<Ws, tungstenite::Error>) -> u16 {
        match r {
            Err(tungstenite::Error::Http(res)) => res.status().as_u16(),
            Err(e) => panic!("expected an HTTP refusal, got {e}"),
            Ok(_) => panic!("expected an HTTP refusal, got an open socket"),
        }
    }

    /// Reads binary frames until their accumulated text contains `needle`.
    async fn read_until(ws: &mut Ws, needle: &str) -> String {
        let mut seen = String::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !seen.contains(needle) {
                match ws.next().await {
                    Some(Ok(WsMsg::Binary(b))) => seen.push_str(&String::from_utf8_lossy(&b)),
                    Some(Ok(_)) => {}
                    other => panic!("socket ended before {needle:?}: {other:?}\n{seen}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; saw:\n{seen}"));
        seen
    }

    /// Drains frames until the close frame and returns its code.
    async fn close_code(ws: &mut Ws) -> u16 {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMsg::Close(Some(f)))) => return u16::from(f.code),
                    Some(Ok(WsMsg::Close(None))) | None => panic!("closed without a code"),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => panic!("socket error before close: {e}"),
                }
            }
        })
        .await
        .expect("no close frame within 5s")
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

    #[tokio::test]
    async fn refusals_happen_before_anything_spawns() {
        let b = serve(cfg_for(tempdir("refuse"))).await.unwrap();
        let ok = "name=cdash-x&cols=80&rows=24";
        assert_eq!(
            status_of(connect(b.addr, ok, Some("https://evil.example")).await),
            403
        );
        assert_eq!(
            status_of(connect(b.addr, "name=other&cols=80&rows=24", None).await),
            400
        );
        assert_eq!(
            status_of(connect(b.addr, "name=cdash-x&cols=0&rows=24", None).await),
            400
        );
    }

    #[test]
    fn attach_command_survives_a_nested_and_non_utf8_environment() {
        // Review focus 1 and 2: an agent started inside tmux inherits TMUX,
        // which names the server `new-session` used, so attach must keep it;
        // an agent without a UTF-8 locale makes the tmux client draw every
        // box character as `_`.
        let cmd = attach_command("cdash-a", "/usr/bin:/bin");
        let std = cmd.as_std();
        let args: Vec<_> = std
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-u", "attach-session", "-t", "=cdash-a"]);
        let envs: Vec<_> = std.get_envs().collect();
        assert!(
            envs.iter().all(|(k, _)| *k != "TMUX"),
            "TMUX is inherited: {envs:?}"
        );
        assert!(envs.contains(&(
            std::ffi::OsStr::new("TERM"),
            Some(std::ffi::OsStr::new("xterm-256color"))
        )));
    }

    #[tokio::test]
    async fn write_all_delivers_every_byte_through_a_full_pipe() {
        // Review focus 3: a paste bigger than the kernel buffer comes back
        // short or EAGAIN from a non-blocking write.
        let (mut r, w) = std::io::pipe().unwrap();
        let w = OwnedFd::from(w);
        rustix::fs::fcntl_setfl(&w, rustix::fs::OFlags::NONBLOCK).unwrap();
        let w = tokio::io::unix::AsyncFd::new(w).unwrap();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut r, &mut out).unwrap();
            out
        });
        write_all(&w, &data).await.unwrap();
        drop(w);
        assert_eq!(reader.join().unwrap(), data);
    }

    #[tokio::test]
    async fn typed_bytes_reach_the_pane_and_its_output_comes_back() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("echo");
        let b = serve(cfg_for(tempdir("echo"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None)
            .await
            .unwrap();
        // Arithmetic, so the echo of the typed line cannot satisfy the wait.
        ws.send(WsMsg::binary(b"echo hi-$((40+2))\r".to_vec()))
            .await
            .unwrap();
        read_until(&mut ws, "hi-42").await;
    }

    #[tokio::test]
    async fn resize_reaches_the_tmux_client() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("resize");
        let b = serve(cfg_for(tempdir("resize"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=60&rows=20", s.0), None)
            .await
            .unwrap();
        eventually("initial size", || s.clients() == "60x20").await;
        ws.send(WsMsg::text(r#"{"resize":[100,30]}"#))
            .await
            .unwrap();
        eventually("resized", || s.clients() == "100x30").await;
    }

    #[tokio::test]
    async fn closing_the_socket_detaches_only_that_client() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("close");
        let b = serve(cfg_for(tempdir("close"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None)
            .await
            .unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        ws.close(None).await.unwrap();
        eventually("client gone", || s.clients().is_empty()).await;
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)])
            .status
            .success());
    }

    #[tokio::test]
    async fn an_unclean_drop_detaches_the_client() {
        // Review focus 4: a phone that sleeps sends no close frame.
        if !have_tmux() {
            return;
        }
        let s = Session::new("drop");
        let b = serve(cfg_for(tempdir("drop"))).await.unwrap();
        let ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None)
            .await
            .unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        drop(ws); // TCP closed, no WebSocket close frame
        eventually("client gone", || s.clients().is_empty()).await;
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)])
            .status
            .success());
    }

    #[tokio::test]
    async fn a_client_that_stops_answering_is_detached() {
        // Review focus 4: a phone that loses its network sends no FIN, so
        // only the missing pong reveals it. This client never reads, so it
        // never answers the bridge's ping.
        if !have_tmux() {
            return;
        }
        let s = Session::new("silent");
        let b = serve(cfg_for(tempdir("silent"))).await.unwrap();
        let _ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None)
            .await
            .unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        eventually("client gone", || s.clients().is_empty()).await;
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)])
            .status
            .success());
    }

    #[tokio::test]
    async fn killing_the_session_closes_with_4000() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("kill");
        let b = serve(cfg_for(tempdir("kill"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None)
            .await
            .unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        tmux(&["kill-session", "-t", &format!("={}", s.0)]);
        assert_eq!(close_code(&mut ws).await, 4000);
    }

    #[tokio::test]
    async fn detaching_from_tmux_closes_with_4001_and_keeps_the_session() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("detach");
        let b = serve(cfg_for(tempdir("detach"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None)
            .await
            .unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        tmux(&["detach-client", "-s", &format!("={}", s.0)]);
        assert_eq!(close_code(&mut ws).await, 4001);
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)])
            .status
            .success());
    }

    #[tokio::test]
    async fn a_prefix_name_does_not_attach_a_longer_session() {
        // Review focus 6: plain `-t cdash-x` would match `cdash-xy`.
        if !have_tmux() {
            return;
        }
        let s = Session::new("prefixlonger");
        let prefix = s.0.trim_end_matches("longer").to_string();
        let b = serve(cfg_for(tempdir("prefix"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={prefix}&cols=80&rows=24"), None)
            .await
            .unwrap();
        assert_eq!(close_code(&mut ws).await, 4000);
        assert!(s.clients().is_empty(), "nothing attached to {}", s.0);
    }
}

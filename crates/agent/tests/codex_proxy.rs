//! The adapter is exercised through the packaged CLI, over a real Unix socket.
use futures_util::{SinkExt, StreamExt};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn proxy_preserves_jsonl_unicode_ping_and_eof() {
    let dir = std::env::temp_dir().join(format!("cdash-proxy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("backend.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    #[allow(clippy::disallowed_types)] // Exercise the real CLI boundary.
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_cdash-agent"))
        .arg("codex-proxy")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .expect("proxy did not connect to the private socket")
        .unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let request = r#"{"id":1,"method":"initialize","params":{"text":"日本語"}}"#;
    input
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_text().unwrap(),
        request
    );
    let reply = r#"{"id":1,"result":{"text":"日本語"}}"#;
    ws.send(Message::text(reply)).await.unwrap();
    assert_eq!(output.next_line().await.unwrap().unwrap(), reply);
    ws.send(Message::Ping(vec![1, 2, 3].into())).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Message::Pong(vec![1, 2, 3].into())
    );
    drop(input);
    assert!(matches!(
        ws.next().await.unwrap().unwrap(),
        Message::Close(_)
    ));
    let _ = ws.close(None).await;
    assert!(tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn proxy_authenticates_to_a_loopback_backend() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    #[allow(clippy::disallowed_types)] // Exercise the real CLI boundary.
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_cdash-agent"))
        .args(["codex-proxy", &url])
        .env("CDASH_CODEX_TOKEN", "test-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .expect("proxy did not connect to the loopback backend")
        .unwrap();
    let mut auth = None;
    let capture = |req: &Request, res: Response| -> Result<Response, ErrorResponse> {
        auth = req.headers().get("authorization").cloned();
        Ok(res)
    };
    let mut ws = tokio_tungstenite::accept_hdr_async(stream, capture).await.unwrap();
    assert_eq!(auth.unwrap().to_str().unwrap(), "Bearer test-token");
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"{\"id\":1}\n").await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap(), r#"{"id":1}"#);
    drop(input);
    assert!(matches!(ws.next().await.unwrap().unwrap(), Message::Close(_)));
    let _ = ws.close(None).await;
    assert!(tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
}

#[tokio::test]
async fn proxy_errors_do_not_start_a_dashboard_server() {
    for args in [
        vec!["codex-proxy"],
        vec!["codex-proxy", "/no/such/cdash/socket"],
        vec!["codex-proxy", "ws://127.0.0.1:9"],
    ] {
        #[allow(clippy::disallowed_types)] // CLI argument and connection error boundary.
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_cdash-agent"))
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "stdout is reserved for RPC");
        assert!(!output.stderr.is_empty());
    }
}

#[tokio::test]
async fn proxy_exits_when_server_closes_with_stdin_still_open() {
    for abrupt in [false, true] {
        let dir =
            std::env::temp_dir().join(format!("cdash-proxy-close-{}-{abrupt}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backend.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        #[allow(clippy::disallowed_types)] // Exercise process exit with a blocked stdin read.
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_cdash-agent"))
            .arg("codex-proxy")
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(b"{}\n").await.unwrap();
        ws.next().await.unwrap().unwrap();
        if !abrupt {
            ws.close(None).await.unwrap();
        }
        drop(ws);
        let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .expect("server closure must exit without waiting for stdin EOF")
            .unwrap();
        assert_eq!(status.success(), !abrupt);
        drop(input);
        let _ = std::fs::remove_dir_all(dir);
    }
}

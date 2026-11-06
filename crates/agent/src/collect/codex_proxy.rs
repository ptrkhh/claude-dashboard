//! JSONL adapter for the bridge's private app-server WebSocket.
use futures_util::{SinkExt, StreamExt};
use std::ffi::OsStr;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Both the standalone agent and in-process Tauri executable support this
/// mode, so Node needs no WebSocket package.
pub fn run_if_requested() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("codex-proxy")) {
        return false;
    }
    let path = match (args.next(), args.next()) {
        (Some(path), None) => path,
        _ => {
            eprintln!("usage: codex-proxy <private Unix socket | ws://127.0.0.1:PORT>");
            std::process::exit(2);
        }
    };
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(forward(&path));
        // Tokio stdin uses an uncancellable blocking read. This process mode
        // must exit on backend closure even while its parent keeps stdin open.
        runtime.shutdown_background();
        result
    })();
    if let Err(error) = result {
        eprintln!("Codex transport: {error}");
        std::process::exit(1);
    }
    true
}

async fn forward(target: &OsStr) -> Result<(), Box<dyn std::error::Error>> {
    // Android's bridge listens on loopback: Codex cannot use Unix sockets there.
    if let Some(url) = target.to_str().filter(|t| t.starts_with("ws://")) {
        let token = std::env::var("CDASH_CODEX_TOKEN")
            .map_err(|_| "CDASH_CODEX_TOKEN is not set")?;
        let mut request = url.into_client_request()?;
        request.headers_mut().insert(AUTHORIZATION, format!("Bearer {token}").parse()?);
        let (socket, _) = tokio_tungstenite::connect_async(request).await?;
        return pump(socket).await;
    }
    let stream = tokio::net::UnixStream::connect(target).await?;
    let (socket, _) = tokio_tungstenite::client_async("ws://localhost/", stream).await?;
    pump(socket).await
}

async fn pump<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: WebSocketStream<S>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    loop {
        tokio::select! {
            line = input.next_line() => match line? {
                Some(line) => socket.send(Message::text(line)).await?,
                None => { socket.close(None).await?; return Ok(()); }
            },
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    output.write_all(text.as_bytes()).await?;
                    output.write_all(b"\n").await?;
                    output.flush().await?;
                }
                Some(Ok(Message::Ping(_))) => socket.flush().await?,
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(Message::Pong(_))) => {},
                Some(Ok(_)) => return Err("expected a text app-server message".into()),
                Some(Err(error)) => return Err(error.into()),
            }
        }
    }
}

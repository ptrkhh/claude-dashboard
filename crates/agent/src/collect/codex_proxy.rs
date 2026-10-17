//! JSONL adapter for the bridge's private app-server Unix WebSocket.
use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

/// Both the standalone agent and in-process Tauri executable support this
/// mode, so Node needs no WebSocket package and no additional network port.
pub fn run_if_requested() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("codex-proxy")) {
        return false;
    }
    let path = match (args.next(), args.next()) {
        (Some(path), None) => path,
        _ => {
            eprintln!("usage: codex-proxy <private Unix socket>");
            std::process::exit(2);
        }
    };
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(forward(Path::new(&path)));
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

async fn forward(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    let (mut socket, _) = tokio_tungstenite::client_async("ws://localhost/", stream).await?;
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

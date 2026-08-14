use cdash_agent::http::serve::{serve, Config};

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    let (bind, port) = (cfg.bind, cfg.port);

    match serve(cfg).await {
        Ok(b) => {
            println!("cdash-agent {} on http://{}", env!("CARGO_PKG_VERSION"), b.addr);
            let missing = b.ctx.host.missing();
            if !missing.is_empty() {
                println!("missing: {}", missing.join(", "));
            }
            // The task inside `serve` owns the accept loop; park here.
            std::future::pending::<()>().await;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // The spec's diagnosed condition: stderr, exit 3, no pidfile.
            eprintln!("port {port} already in use");
            std::process::exit(3);
        }
        Err(e) => {
            eprintln!("cannot bind {bind}:{port}: {e}");
            std::process::exit(1);
        }
    }
}

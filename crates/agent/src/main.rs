use cdash_agent::http::serve::{serve, Config};

/// Prints the hash to stdout for the operator to place in the environment. It
/// never writes a file: this process serves `/api/browse` and `/api/logs`, so
/// a secret on its disk is one disclosure away from total compromise.
fn read_password_twice() -> Result<String, String> {
    let a = prompt_hidden("Password: ")?;
    let b = prompt_hidden("Again: ")?;
    if a != b {
        return Err("passwords did not match".to_string());
    }
    cdash_agent::auth::password::hash_password(&a)
}

/// Echo suppression via termios, so the password never reaches the terminal
/// scrollback. Falls back to an unsuppressed read when stdin is not a tty
/// (a pipe), which is what makes the subcommand scriptable.
fn prompt_hidden(prompt: &str) -> Result<String, String> {
    use std::io::{BufRead, Write};
    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    let tty = std::io::stdin();
    let saved = rustix::termios::tcgetattr(&tty).ok();
    if let Some(s) = &saved {
        let mut raw = s.clone();
        raw.local_modes -= rustix::termios::LocalModes::ECHO;
        let _ = rustix::termios::tcsetattr(&tty, rustix::termios::OptionalActions::Now, &raw);
    }

    let mut line = String::new();
    let read = tty.lock().read_line(&mut line);
    // Restore the terminal whatever happened.
    if let Some(s) = &saved {
        let _ = rustix::termios::tcsetattr(&tty, rustix::termios::OptionalActions::Now, s);
        eprintln!();
    }
    read.map_err(|e| e.to_string())?;

    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("set-password") {
        match read_password_twice() {
            Ok(hash) => {
                println!("{hash}");
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }

    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            // A misconfiguration that would otherwise open the origin is
            // refused at boot rather than debugged in production.
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
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
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            // A startup refusal, e.g. cf-access could not obtain its keys.
            // Named, non-zero, and nothing ever listened.
            eprintln!("{e}");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("cannot bind {bind}:{port}: {e}");
            std::process::exit(1);
        }
    }
}

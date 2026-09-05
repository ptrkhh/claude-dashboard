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
#[cfg(unix)]
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

/// Echo suppression via the console mode. When stdin is not a console (a
/// pipe), `GetConsoleMode` fails and the read is unsuppressed, which is what
/// keeps the subcommand scriptable — the same fallback as the termios path.
#[cfg(windows)]
fn prompt_hidden(prompt: &str) -> Result<String, String> {
    use std::io::{BufRead, Write};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, STD_INPUT_HANDLE,
    };
    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    // SAFETY: plain Win32 calls on the process's own stdin handle; a null or
    // invalid handle makes GetConsoleMode return 0 and nothing is changed.
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0u32;
    let saved = (unsafe { GetConsoleMode(handle, &mut mode) } != 0).then_some(mode);
    if let Some(m) = saved {
        unsafe { SetConsoleMode(handle, m & !ENABLE_ECHO_INPUT) };
    }

    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if let Some(m) = saved {
        unsafe { SetConsoleMode(handle, m) };
        eprintln!();
    }
    read.map_err(|e| e.to_string())?;

    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

#[tokio::main]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("set-password") => match read_password_twice() {
            Ok(hash) => {
                println!("{hash}");
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        // Task Scheduler registration of the windowless twin (spec §5).
        #[cfg(windows)]
        Some(cmd @ ("install" | "uninstall")) => {
            use cdash_agent::host::cmd::Runner;
            use cdash_agent::host::log::LogBuffer;
            let runner = Runner::new(
                std::env::var("PATH").unwrap_or_default(),
                std::sync::Arc::new(LogBuffer::new()),
            );
            let done = if cmd == "install" {
                cdash_agent::host::task::install(&runner).await
            } else {
                cdash_agent::host::task::uninstall(&runner).await
            };
            match done {
                Ok(msg) => {
                    println!("{msg}");
                    return;
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            }
        }
        // Without this, `cdash-agent --version` binds a port and parks forever.
        Some(other) => {
            let extra = if cfg!(windows) { "|install|uninstall" } else { "" };
            eprintln!("unknown argument: {other}\nusage: cdash-agent [set-password{extra}]");
            std::process::exit(2);
        }
    }

    cdash_agent::http::serve::serve_from_env().await;
}

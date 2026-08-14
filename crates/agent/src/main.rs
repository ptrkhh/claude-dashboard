use cdash_agent::host;

#[tokio::main]
async fn main() {
    let h = host::init::init().await;
    println!("cdash-agent {}", env!("CARGO_PKG_VERSION"));
    println!("PATH: {}", h.path);
    let missing = h.missing();
    if missing.is_empty() {
        println!("all required binaries found");
    } else {
        println!("missing: {}", missing.join(", "));
    }
}

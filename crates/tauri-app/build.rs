/// `inproc_agent` marks the platforms that link `cdash-agent` and run it in
/// process — Linux and macOS. Windows and Android are thin clients: the agent
/// runs in a WSL distro or in Termux, and this crate only speaks HTTP to it.
/// Keep this in step with the `cdash-agent` dependency's target in Cargo.toml.
///
/// `CDASH_AGENT_BIN`, when set to the path of an `aarch64-unknown-linux-musl`
/// `cdash-agent`, embeds that binary for the Android setup screen to hand to
/// Termux. Unset, the app builds without it and the setup screen says so.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(inproc_agent)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "windows" && os != "android" {
        println!("cargo::rustc-cfg=inproc_agent");
    }
    println!("cargo::rerun-if-env-changed=CDASH_AGENT_BIN");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("cdash-agent.bin");
    match std::env::var_os("CDASH_AGENT_BIN") {
        // Copied rather than `include_bytes!`d from its own path so that a
        // build without it still compiles — the empty file is what the setup
        // screen reports as "no agent bundled".
        Some(src) => std::fs::copy(&src, &out).map(drop),
        None => std::fs::write(&out, []),
    }
    .expect("staging the bundled agent");
    tauri_build::build()
}

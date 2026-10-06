#[cfg(inproc_agent)]
use cdash_agent::auth::config::{AuthConfig, GuardKind};
#[cfg(inproc_agent)]
use cdash_agent::http::serve::Bound;
#[cfg(inproc_agent)]
use std::net::IpAddr;
#[cfg(inproc_agent)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Thin-client platforms link no agent, so nothing there can construct a
/// `Bound`. The commands keep their signatures across platforms; only the
/// bodies differ.
#[cfg(not(inproc_agent))]
pub enum Bound {}

/// The agent's own default, duplicated because the thin-client platforms do
/// not link the agent at all. The assertion below keeps the copy honest: on
/// every platform that *does* link it, a mismatch fails the build.
const LOCAL_AGENT_PORT: u16 = 23274;
#[cfg(inproc_agent)]
const _: () = assert!(LOCAL_AGENT_PORT == cdash_agent::http::serve::DEFAULT_PORT);

/// Where a thin client looks when no profile names another agent. WSL2 relays a
/// loopback listener in the distro to the Windows host on the same port, and
/// Android does not isolate loopback between apps, so on both the agent's own
/// port is simply reachable.
fn local_agent_base() -> String {
    format!("http://localhost:{LOCAL_AGENT_PORT}")
}

#[derive(Default)]
pub struct ServerState(Mutex<Option<Bound>>);

#[cfg(inproc_agent)]
fn url(b: &Bound) -> String {
    format!("http://{}", b.addr)
}

#[cfg(inproc_agent)]
/// The in-process trust shape: loopback only, no auth guard, ephemeral port.
/// Mirrors the agent crate's own test config.
fn server_config() -> cdash_agent::http::serve::Config {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    cdash_agent::http::serve::Config {
        bind: "127.0.0.1".parse::<IpAddr>().expect("literal is a valid IP"),
        port: 0, // OS chooses; the bound address is the readiness signal
        claude_dir: PathBuf::from(home).join(".claude"),
        disk_extra: None,
        public_dir: PathBuf::from("public"),
        auth: Arc::new(
            AuthConfig::build(vec![GuardKind::None], None, String::new(), vec![])
                .expect("none is always buildable"),
        ),
        password: None,
    }
}

#[cfg(inproc_agent)]
fn start_locked(state: &Mutex<Option<Bound>>) -> Result<String, String> {
    let mut guard = state.lock().map_err(|_| "server state poisoned")?;
    if let Some(b) = guard.as_ref() {
        return Ok(url(b)); // idempotent
    }
    let b = tauri::async_runtime::block_on(cdash_agent::http::serve::serve(server_config()))
        .map_err(|e| format!("cannot start agent server: {e}"))?;
    let addr = url(&b);
    *guard = Some(b);
    Ok(addr)
}

#[cfg(inproc_agent)]
fn stop_locked(state: &Mutex<Option<Bound>>) -> Result<(), String> {
    let mut guard = state.lock().map_err(|_| "server state poisoned")?;
    if let Some(b) = guard.take() {
        tauri::async_runtime::block_on(b.stop());
    }
    Ok(())
}

#[cfg(inproc_agent)]
/// `None` once the serving task is gone, not merely once `stop` was called:
/// a panicked accept loop must not keep reporting a live address while every
/// request to it is refused.
fn addr_locked(state: &Mutex<Option<Bound>>) -> Result<Option<String>, String> {
    let mut guard = state.lock().map_err(|_| "server state poisoned")?;
    if guard.as_ref().is_some_and(Bound::is_finished) {
        *guard = None;
    }
    Ok(guard.as_ref().map(url))
}

/// What a thin client says instead of starting a server. Everything the agent
/// drives — tmux, `claude`, `/proc` — lives in the WSL distro or in Termux;
/// `host_platform` tells the UI which setup instructions to show.
#[cfg(not(inproc_agent))]
fn start_locked(_state: &Mutex<Option<Bound>>) -> Result<String, String> {
    Err(format!(
        "this build has no in-process agent: run cdash-agent in WSL or Termux on port \
         {LOCAL_AGENT_PORT}, and the client will reach it at {}",
        local_agent_base()
    ))
}

#[cfg(not(inproc_agent))]
fn stop_locked(_state: &Mutex<Option<Bound>>) -> Result<(), String> {
    Ok(())
}

#[cfg(not(inproc_agent))]
fn addr_locked(_state: &Mutex<Option<Bound>>) -> Result<Option<String>, String> {
    Ok(None)
}

#[tauri::command]
fn server_start(state: tauri::State<ServerState>) -> Result<String, String> {
    start_locked(&state.0)
}

#[tauri::command]
fn server_stop(state: tauri::State<ServerState>) -> Result<(), String> {
    stop_locked(&state.0)
}

#[tauri::command]
fn server_state(state: tauri::State<ServerState>) -> Result<Option<String>, String> {
    addr_locked(&state.0)
}

pub struct ReqwestState(reqwest::Client);

#[derive(serde::Serialize, Debug)]
struct ApiResponse {
    status: u16,
    body: serde_json::Value,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct ProfileRecord {
    name: String,
    base_url: String,
    /// "in-process" | "external"
    managed: String,
    auth: String,
    has_secret: bool,
}

#[derive(serde::Deserialize, Debug)]
struct ProfileInput {
    name: String,
    base_url: String,
    managed: String,
    auth: String,
}

fn validate_profile(input: &ProfileInput) -> Result<(), String> {
    if input.name.trim().is_empty() {
        return Err("profile name must not be empty".into());
    }
    if input.managed != "in-process" && input.managed != "external" {
        return Err("managed must be \"in-process\" or \"external\"".into());
    }
    // The client sends requests wherever this points, so refuse anything that is
    // not plainly a web address before it is stored.
    match reqwest::Url::parse(&input.base_url) {
        // No credentials (they would sit in the store in clear, sidestepping
        // the "auth none only" rule below), no query or fragment (the path is
        // appended to this string).
        Ok(u) if matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none() => {}
        _ => return Err(format!("base_url {:?} is not an http(s) URL", input.base_url)),
    }
    if input.auth != "none" {
        // Fail closed: a secret-bearing profile must never be silently accepted.
        return Err(format!(
            "auth {:?} is not supported until step 10 wires the keyring; only \"none\" is accepted",
            input.auth
        ));
    }
    Ok(())
}

/// The store logic in plain form so tests can run it headlessly: the
/// "profiles" document is a JSON map keyed by profile name.
type ProfilesDoc = serde_json::Map<String, serde_json::Value>;

fn profile_upsert(profiles: &mut ProfilesDoc, input: ProfileInput) -> Result<(), String> {
    validate_profile(&input)?;
    let rec = ProfileRecord {
        name: input.name,
        base_url: input.base_url,
        managed: input.managed,
        auth: input.auth,
        has_secret: false, // step 10 wires the keyring
    };
    profiles.insert(rec.name.clone(), serde_json::to_value(&rec).expect("serializable"));
    Ok(())
}

/// The delete command's whole logic: removing the active profile clears
/// "active"; anything else leaves it untouched.
fn delete_profile(profiles: &mut ProfilesDoc, active: &mut Option<String>, name: &str) {
    if active.as_deref() == Some(name) {
        *active = None;
    }
    profiles.remove(name);
}

/// Store-value parsing for the "profiles" key: an absent or non-object value
/// means no profiles.
fn doc_from_value(value: Option<&serde_json::Value>) -> ProfilesDoc {
    value.and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

fn profile_records(profiles: &ProfilesDoc) -> Vec<ProfileRecord> {
    let mut out: Vec<ProfileRecord> = profiles
        .values()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn open_store<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<Arc<tauri_plugin_store::Store<R>>, String> {
    use tauri_plugin_store::StoreExt;
    app.store("profiles.json").map_err(|e| format!("store unavailable: {e}"))
}

fn profiles_doc<R: tauri::Runtime>(store: &tauri_plugin_store::Store<R>) -> ProfilesDoc {
    doc_from_value(store.get("profiles").as_ref())
}

/// No-op passthrough until step 10 wires bearer tokens from the keyring.
fn attach_auth(
    _profile: Option<&ProfileRecord>,
    req: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    req
}

/// Resolves the active profile name against the stored records; a stale
/// "active" (pointing at a missing record) resolves to None.
fn resolve_active(profiles: &ProfilesDoc, active: Option<&str>) -> Option<ProfileRecord> {
    profiles
        .get(active?)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

fn active_record<R: tauri::Runtime>(store: &tauri_plugin_store::Store<R>) -> Option<ProfileRecord> {
    let active = store.get("active")?.as_str()?.to_string();
    resolve_active(&profiles_doc(store), Some(&active))
}

/// Where API calls go: the in-process agent when there is one, else the active
/// profile, else — on a thin client, which never has one — the local default.
fn base_url(inproc: Option<String>, active: Option<&ProfileRecord>) -> Option<String> {
    inproc
        .or_else(|| active.map(|p| p.base_url.trim_end_matches('/').to_string()))
        .or_else(|| (!cfg!(inproc_agent)).then(local_agent_base))
}

/// The entire data path: JS names a path, we resolve it against the address
/// the caller picked — the in-process agent's loopback address, else the active
/// profile's validated http(s) `base_url`, else the local default on a thin
/// client. No cookie jar; the client is built once.
async fn request_inner(
    http: &reqwest::Client,
    addr: Option<String>,
    active: Option<&ProfileRecord>,
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<ApiResponse, String> {
    let addr = addr.ok_or("server not running")?;
    if !path.starts_with('/') {
        return Err("path must be absolute".into());
    }
    let url = format!("{addr}{path}");
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let req = attach_auth(active, http.request(m, &url));
    let req = if let Some(b) = body { req.json(&b) } else { req };
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let body = res.json::<serde_json::Value>().await.unwrap_or(serde_json::Value::Null);
    Ok(ApiResponse { status, body })
}

#[tauri::command]
async fn api_request(
    app: tauri::AppHandle,
    state: tauri::State<'_, ServerState>,
    http: tauri::State<'_, ReqwestState>,
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<ApiResponse, String> {
    let inproc = addr_locked(&state.0)?;
    let active = open_store(&app).ok().and_then(|s| active_record(&s));
    let addr = base_url(inproc, active.as_ref());
    request_inner(&http.0, addr, active.as_ref(), method, path, body).await
}

#[tauri::command]
fn profiles_list<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<Vec<ProfileRecord>, String> {
    let store = open_store(&app)?;
    Ok(profile_records(&profiles_doc(&store)))
}

#[tauri::command]
fn profile_save<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    profile: ProfileInput,
) -> Result<(), String> {
    let store = open_store(&app)?;
    let mut doc = profiles_doc(&store);
    profile_upsert(&mut doc, profile)?;
    store.set("profiles", serde_json::Value::Object(doc));
    store.save().map_err(|e| format!("cannot persist profile: {e}"))
}

#[tauri::command]
fn profile_delete<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    name: String,
) -> Result<(), String> {
    let store = open_store(&app)?;
    let mut doc = profiles_doc(&store);
    let mut active = store.get("active").and_then(|v| v.as_str().map(str::to_string));
    delete_profile(&mut doc, &mut active, &name);
    store.set("profiles", serde_json::Value::Object(doc));
    store.set("active", active.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null));
    store.save().map_err(|e| format!("cannot persist profiles: {e}"))
}

#[tauri::command]
fn profile_activate<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    name: String,
) -> Result<(), String> {
    let store = open_store(&app)?;
    if !profiles_doc(&store).contains_key(&name) {
        return Err(format!("unknown profile {name:?}"));
    }
    store.set("active", name);
    store.save().map_err(|e| format!("cannot persist active profile: {e}"))
}

/// Hands the bundled agent to Termux over loopback. Android does not isolate
/// loopback between apps, so one `curl` line from Termux can pull the binary
/// straight out of this process — where every route through shared storage
/// needs MediaStore or a permission Termux cannot hold on Android 11+.
///
/// What it serves is a public release artifact, and it serves it to any app on
/// the phone, so this starts only when the setup screen asks for it — not at
/// launch.
mod handoff {
    #[cfg(any(not(windows), test))]
    use std::io::{Read, Write};
    #[cfg(any(not(windows), test))]
    use std::net::{TcpListener, TcpStream};
    #[cfg(any(not(windows), test))]
    use std::sync::Mutex;
    #[cfg(any(not(windows), test))]
    use std::time::Duration;

    /// Any app on the phone can connect here, and the accept loop serves one
    /// connection at a time: without a bound, a client that connects and says
    /// nothing would starve the one fetch that matters. Loopback moves 9 MB in
    /// milliseconds, so this is generous.
    #[cfg(any(not(windows), test))]
    const IO_TIMEOUT: Duration = Duration::from_secs(5);

    /// The `aarch64-unknown-linux-musl` agent, embedded when `CDASH_AGENT_BIN`
    /// named one at build time; empty otherwise.
    pub const AGENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cdash-agent.bin"));

    #[cfg(any(not(windows), test))]
    static URL: Mutex<Option<String>> = Mutex::new(None);

    #[cfg(not(windows))]
    pub fn start() -> Result<String, String> {
        start_serving(AGENT)
    }

    /// Idempotent: the second call returns the first listener's URL rather than
    /// binding a second port.
    #[cfg(any(not(windows), test))]
    pub fn start_serving(body: &'static [u8]) -> Result<String, String> {
        if body.is_empty() {
            return Err("this build bundles no agent: rebuild with CDASH_AGENT_BIN set".into());
        }
        let mut url = URL.lock().map_err(|_| "handoff state poisoned")?;
        if let Some(u) = url.as_ref() {
            return Ok(u.clone());
        }
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        std::thread::spawn(move || {
            // One connection at a time is right for a file fetched once by
            // hand; `flatten` drops connections that failed to accept.
            for conn in listener.incoming().flatten() {
                let _ = respond(conn, body, IO_TIMEOUT);
            }
        });
        let addr = format!("http://127.0.0.1:{port}/cdash-agent");
        *url = Some(addr.clone());
        Ok(addr)
    }

    /// The request is read and discarded: one file, one response, whatever was
    /// asked for. Reading it at all is what keeps the client from seeing a
    /// reset instead of the body.
    #[cfg(any(not(windows), test))]
    pub(super) fn respond(mut conn: TcpStream, body: &[u8], timeout: Duration) -> std::io::Result<()> {
        conn.set_read_timeout(Some(timeout))?;
        conn.set_write_timeout(Some(timeout))?;
        let mut scratch = [0u8; 1024];
        let _ = conn.read(&mut scratch)?;
        let head = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        conn.write_all(head.as_bytes())?;
        conn.write_all(body)
    }
}

/// Where WSL can read a Windows path: `C:\x\y` and the verbatim `\\?\C:\x\y`
/// both become `/mnt/c/x/y`, since WSL mounts the drives there by default.
/// A copy needs no network, which is the point — under WSL2's default NAT
/// networking the distro's `127.0.0.1` is its own, not the host's.
#[cfg(any(windows, test))] // Windows calls it; the tests pin it on every platform
fn wsl_path(windows_path: &str) -> Option<String> {
    // `canonicalize` hands back verbatim paths on Windows; strip that prefix
    // first or the drive letter is never found.
    let p = windows_path.strip_prefix(r"\\?\").unwrap_or(windows_path);
    let (drive, rest) = p.split_once(':')?;
    let mut letters = drive.chars();
    let letter = letters.next()?;
    if letters.next().is_some() || !letter.is_ascii_alphabetic() {
        return None; // a UNC share or "CD:" is nothing WSL mounts under /mnt
    }
    let rest = rest.trim_start_matches(['\\', '/']).replace('\\', "/");
    Some(format!("/mnt/{}/{}", letter.to_ascii_lowercase(), rest))
}

/// Writes the bundled agent where the other side can read it, and only when it
/// is not already there byte for byte: the setup command is pasted more than
/// once by design, and there is no point rewriting 9 MB that a `cp` in WSL may
/// be reading at that moment.
#[cfg(any(windows, test))] // as above
fn export_agent(dir: &std::path::Path, body: &[u8]) -> Result<std::path::PathBuf, String> {
    if body.is_empty() {
        return Err("this build bundles no agent: rebuild with CDASH_AGENT_BIN set".into());
    }
    let path = dir.join("cdash-agent");
    if std::fs::read(&path).is_ok_and(|on_disk| on_disk == body) {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// How this platform hands the agent over, plus the port the setup command
/// health-checks — the number lives here, not in the JavaScript.
///
/// Android curls it out of this process over loopback, which Android does not
/// isolate between apps. Windows cannot do that: binding a routable interface
/// so the distro could reach it would serve the binary to the whole network.
/// So Windows writes the file and lets WSL read it through `/mnt/c`.
#[tauri::command]
fn agent_handoff(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let _ = &app;
    #[cfg(windows)]
    {
        // %TEMP%, not app data: the copy is staging, dead the moment WSL has
        // it, and leaving 9 MB in AppData for an app with no uninstaller is
        // rude. Re-exported whenever the dialog opens, so a cleaned temp heals
        // itself.
        let dir = std::env::temp_dir().join("cdash");
        let path = export_agent(&dir, handoff::AGENT)?;
        // %TEMP% is often an 8.3 short path (C:\Users\ADALOV~1\…); canonicalize
        // returns the long, verbatim form that `wsl_path` is written to expect.
        let path = std::fs::canonicalize(&path)
            .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
        let win = path.to_string_lossy().to_string();
        let wsl = wsl_path(&win)
            .ok_or_else(|| format!("cannot map {win} into WSL; copy it across yourself"))?;
        Ok(serde_json::json!({
            "kind": "copy",
            "source": wsl,
            "bytes": handoff::AGENT.len(),
            "port": LOCAL_AGENT_PORT,
        }))
    }
    #[cfg(not(windows))]
    Ok(serde_json::json!({
        "kind": "curl",
        "source": handoff::start()?,
        "bytes": handoff::AGENT.len(),
        "port": LOCAL_AGENT_PORT,
    }))
}

/// The trust boundary for [`open_external`]. `a.href` comes from our own page,
/// but it is built from `bridgeSessionId` — a value read off disk in
/// `~/.claude/sessions/*.json` — so `file://`, `javascript:` and `intent://`
/// must not be reachable through the opener.
fn is_web_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Hand a URL to the OS instead of the webview. `target="_blank"` is inert in
/// wry — an Android app link like `https://claude.ai/code/<id>` loads inside
/// the webview (signed out, no intent resolution) rather than reaching the
/// Claude app. Only the opener sees the OS.
///
/// An app command, not a direct `opener` plugin call from JS: plugin commands
/// need a capabilities file, app commands do not, so this stays a three-line
/// change instead of a new permissions tree.
#[tauri::command]
fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !is_web_url(&url) {
        return Err(format!("refusing to open non-http(s) url: {url:?}"));
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

/// SHA-256 of the bundled agent, hex — the digest `/api/hostinfo` reports as
/// `build` for a running one. The crate version is the same across rebuilds, so
/// the two digests are how the app tells whether the agent in WSL or Termux is
/// the one it carries.
fn build_id(agent: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(agent))
}

#[tauri::command]
fn agent_build() -> Result<String, String> {
    if handoff::AGENT.is_empty() {
        return Err("this build bundles no agent: rebuild with CDASH_AGENT_BIN set".into());
    }
    Ok(build_id(handoff::AGENT))
}

#[tauri::command]
fn host_platform() -> String {
    std::env::consts::OS.to_string()
}

/// The whole app. A library, not just a `main`, because Android loads it as a
/// shared object: `mobile_entry_point` is what the generated Kotlin activity
/// calls, and `src/main.rs` is a one-line desktop wrapper around the same code.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .manage(ServerState::default())
        // No redirects: a redirect is the only way a request to the address the
        // caller chose (the in-process agent, or the active profile's host)
        // could end up somewhere else.
        // Timeouts because a request that never returns leaves the UI reading
        // "Connecting…" with no way back — the poll ladder only advances when a
        // tick finishes, whether it succeeded or failed.
        .manage(ReqwestState(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(3))
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("a client with no TLS roots to load always builds"),
        ))
        // On Linux and macOS the client *is* the server. Nothing else starts
        // it, so without this every api_request answers "server not running".
        // Runs on the main thread before the event loop, which is what makes
        // start_locked's block_on legal.
        .setup(|_app| {
            #[cfg(inproc_agent)]
            {
                use tauri::Manager;
                start_locked(&_app.state::<ServerState>().0).map_err(std::io::Error::other)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            server_start,
            server_stop,
            server_state,
            api_request,
            profiles_list,
            profile_save,
            profile_delete,
            profile_activate,
            agent_handoff,
            agent_build,
            host_platform,
            open_external
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    // Thin clients (Windows, Android) link no agent, so `start_locked` is an
    // error there by design; the tests that need a live server stay off them.
    #[cfg(inproc_agent)]
    #[test]
    fn start_returns_loopback_url_and_is_idempotent() {
        let state = Mutex::new(None);
        let first = start_locked(&state).unwrap();
        assert!(first.starts_with("http://127.0.0.1:"));
        let second = start_locked(&state).unwrap();
        assert_eq!(first, second, "a second start must return the same address");
        assert_eq!(addr_locked(&state).unwrap(), Some(first));
        stop_locked(&state).unwrap();
        assert_eq!(addr_locked(&state).unwrap(), None, "stop must clear the state");
    }

    #[cfg(inproc_agent)]
    #[test]
    fn stop_then_start_binds_again() {
        let state = Mutex::new(None);
        start_locked(&state).unwrap();
        stop_locked(&state).unwrap();
        assert_eq!(addr_locked(&state).unwrap(), None);
        // The kernel may hand back the same ephemeral port, so the assertion
        // is that a second bind succeeds at all — not that it differs.
        let second = start_locked(&state).unwrap();
        assert!(second.starts_with("http://127.0.0.1:"));
        stop_locked(&state).unwrap();
    }

    /// `start_locked`/`stop_locked` block on tauri's own runtime; they must
    /// not run inside this test's tokio context.
    #[cfg(inproc_agent)]
    fn boot() -> (Mutex<Option<Bound>>, String) {
        let state = Mutex::new(None);
        let addr = std::thread::scope(|s| s.spawn(|| start_locked(&state).unwrap()).join().unwrap());
        (state, addr)
    }

    #[cfg(inproc_agent)]
    fn shutdown(state: &Mutex<Option<Bound>>) {
        std::thread::scope(|s| s.spawn(|| stop_locked(state).unwrap()).join().unwrap());
    }

    #[cfg(inproc_agent)]
    #[tokio::test]
    async fn request_round_trips_health() {
        let (state, addr) = boot();
        let http = reqwest::Client::new();
        let res = request_inner(&http, Some(addr), None, "GET".into(), "/api/health".into(), None)
            .await
            .expect("health round trip");
        assert_eq!(res.status, 200);
        assert_eq!(res.body["ok"], serde_json::Value::Bool(true));
        shutdown(&state);
    }

    #[tokio::test]
    async fn relative_paths_are_rejected() {
        // Rejected before any request is made, so nothing needs to be listening.
        let http = reqwest::Client::new();
        let addr = Some("http://127.0.0.1:9".to_string());
        let err = request_inner(&http, addr, None, "GET".into(), "api/x".into(), None)
            .await
            .unwrap_err();
        assert!(err.contains("absolute"), "got: {err}");
    }

    #[tokio::test]
    async fn requests_without_a_running_server_fail() {
        let http = reqwest::Client::new();
        let err = request_inner(&http, None, None, "GET".into(), "/api/health".into(), None)
            .await
            .unwrap_err();
        assert_eq!(err, "server not running");
    }

    fn input(name: &str) -> ProfileInput {
        ProfileInput {
            name: name.into(),
            base_url: "http://127.0.0.1".into(),
            managed: "in-process".into(),
            auth: "none".into(),
        }
    }

    #[test]
    fn open_external_only_accepts_http_and_https() {
        assert!(is_web_url("https://claude.ai/code/session_01ABC"));
        assert!(is_web_url("http://127.0.0.1:8080/"));
        // The href is built from a bridgeSessionId read off disk, so the
        // schemes that would turn the opener into a local-exec primitive
        // must stay unreachable.
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("intent://claude.ai/code/x#Intent;end"));
        assert!(!is_web_url("HTTPS://claude.ai"), "scheme match is case-sensitive by design");
        assert!(!is_web_url(""));
    }

    #[test]
    fn wsl_paths_are_mapped_under_mnt() {
        assert_eq!(
            wsl_path(r"C:\Users\me\AppData\Local\dev.cdash.app\cdash-agent").as_deref(),
            Some("/mnt/c/Users/me/AppData/Local/dev.cdash.app/cdash-agent")
        );
        // what canonicalize actually returns on Windows
        assert_eq!(
            wsl_path(r"\\?\C:\Users\me\cdash-agent").as_deref(),
            Some("/mnt/c/Users/me/cdash-agent")
        );
        // a lowercase drive, and one already using forward slashes
        assert_eq!(wsl_path(r"d:\tools\x").as_deref(), Some("/mnt/d/tools/x"));
        assert_eq!(wsl_path("E:/tools/x").as_deref(), Some("/mnt/e/tools/x"));
        // spaces survive: the command quotes the path, and usernames have them
        assert_eq!(
            wsl_path(r"C:\Users\Ada Lovelace\x").as_deref(),
            Some("/mnt/c/Users/Ada Lovelace/x")
        );

        // nothing WSL mounts under /mnt
        assert_eq!(wsl_path(r"\\server\share\x"), None);
        assert_eq!(wsl_path(r"CD:\x"), None);
        assert_eq!(wsl_path("/already/unix"), None);
        assert_eq!(wsl_path(""), None);
    }

    #[test]
    fn build_id_is_the_lowercase_sha256_hex_the_agent_reports() {
        // The agent formats its own digest the same way (`build_id` in its
        // routes.rs); a different case or truncation would never compare equal.
        assert_eq!(
            build_id(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn export_writes_once_and_refuses_an_empty_bundle() {
        let dir = std::env::temp_dir().join(format!("cdash-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(export_agent(&dir, b"").unwrap_err().contains("CDASH_AGENT_BIN"));
        assert!(!dir.exists(), "an empty bundle writes nothing at all");

        let path = export_agent(&dir, b"agent v1").expect("writes");
        assert_eq!(std::fs::read(&path).unwrap(), b"agent v1");

        // Pasting the command again must not rewrite an identical file — a `cp`
        // in WSL may be reading it.
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(export_agent(&dir, b"agent v1").unwrap(), path);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);

        // a new version does replace it
        export_agent(&dir, b"agent v2").expect("rewrites");
        assert_eq!(std::fs::read(&path).unwrap(), b"agent v2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handoff_serves_the_bundled_bytes_once_started() {
        use std::io::{Read, Write};

        // An empty bundle must not open a port at all.
        assert!(handoff::start_serving(b"").unwrap_err().contains("CDASH_AGENT_BIN"));

        const BODY: &[u8] = b"\x7fELF not really, but the server never looks";
        let url = handoff::start_serving(BODY).expect("binds loopback");
        assert_eq!(handoff::start_serving(BODY).unwrap(), url, "second start reuses the port");

        let addr = url.trim_start_matches("http://").split('/').next().unwrap().to_string();
        let mut sock = std::net::TcpStream::connect(&addr).expect("connects");
        sock.write_all(b"GET /cdash-agent HTTP/1.0\r\n\r\n").unwrap();
        let mut got = Vec::new();
        sock.read_to_end(&mut got).unwrap();

        let split = got.windows(4).position(|w| w == b"\r\n\r\n").expect("headers end");
        let (head, body) = got.split_at(split + 4);
        let head = String::from_utf8_lossy(head);
        assert!(head.starts_with("HTTP/1.0 200 OK"), "got: {head}");
        assert!(head.contains(&format!("Content-Length: {}", BODY.len())), "got: {head}");
        assert_eq!(body, BODY);
    }

    #[test]
    fn a_silent_client_cannot_hold_the_handoff_open() {
        use std::net::{TcpListener, TcpStream};
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        // Connects and never sends a byte: what a stray app on the phone can do.
        let _silent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (conn, _) = listener.accept().unwrap();

        // On a thread, so a missing timeout fails this test instead of hanging CI.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(handoff::respond(conn, b"agent", Duration::from_millis(100)));
        });
        let started = Instant::now();
        let outcome = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("respond must return once its read times out, not wait for a request");
        assert!(outcome.is_err(), "a client that sent nothing gets dropped");
        assert!(started.elapsed() < Duration::from_secs(2), "and promptly");
    }

    #[test]
    fn profiles_must_point_at_a_web_address() {
        let mut doc = ProfilesDoc::new();
        for ok in ["http://127.0.0.1:23274", "https://dash.example.com/base"] {
            let mut p = input("ok");
            p.base_url = ok.into();
            profile_upsert(&mut doc, p).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "", "localhost:23274", "file:///etc/passwd", "ftp://host/", "javascript:alert(1)", "http://",
            "http://user:pw@host", "http://user@host", "http://host/?x=1", "http://host/#frag",
        ] {
            let mut p = input("bad");
            p.base_url = bad.into();
            assert!(profile_upsert(&mut doc, p).unwrap_err().contains("http(s)"), "{bad:?}");
        }
        assert!(!doc.contains_key("bad"), "a refused profile is not stored");
    }

    #[test]
    fn base_url_prefers_the_in_process_agent_then_the_active_profile() {
        let mut doc = ProfilesDoc::new();
        let mut trailing = input("remote");
        trailing.base_url = "http://192.168.1.9:8080/".into();
        profile_upsert(&mut doc, trailing).unwrap();
        let rec = resolve_active(&doc, Some("remote")).unwrap();

        let inproc = Some("http://127.0.0.1:41234".to_string());
        assert_eq!(base_url(inproc.clone(), Some(&rec)).as_deref(), Some("http://127.0.0.1:41234"));
        assert_eq!(base_url(inproc, None).as_deref(), Some("http://127.0.0.1:41234"));
        // the profile is the fallback, and its trailing slash must not survive
        // into "http://host:8080//api/health"
        assert_eq!(base_url(None, Some(&rec)).as_deref(), Some("http://192.168.1.9:8080"));
        // nothing to talk to: only the Windows client, which never has an
        // in-process agent, has a default worth guessing.
        assert_eq!(base_url(None, None).is_some(), !cfg!(inproc_agent));
    }

    #[test]
    fn profile_save_list_delete_round_trip() {
        let mut doc = ProfilesDoc::new();
        profile_upsert(&mut doc, input("local")).unwrap();
        profile_upsert(&mut doc, input("remote")).unwrap();

        let listed = profile_records(&doc);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "local"); // sorted
        assert_eq!(listed[0].auth, "none");
        assert!(!listed[0].has_secret);

        // upsert overwrites by name, never duplicates
        let mut updated = input("local");
        updated.base_url = "http://127.0.0.1:9999".into();
        profile_upsert(&mut doc, updated).unwrap();
        assert_eq!(profile_records(&doc).len(), 2);
        assert_eq!(profile_records(&doc)[0].base_url, "http://127.0.0.1:9999");

        doc.remove("remote");
        assert_eq!(profile_records(&doc).len(), 1);
        doc.remove("missing"); // delete of unknown is a no-op
        assert_eq!(profile_records(&doc).len(), 1);
    }

    #[test]
    fn secret_bearing_profiles_are_rejected_naming_step_10() {
        let mut doc = ProfilesDoc::new();
        let mut bad = input("secret");
        bad.auth = "bearer".into();
        let err = profile_upsert(&mut doc, bad).unwrap_err();
        assert!(err.contains("step 10"), "got: {err}");
        assert!(doc.is_empty(), "fail closed: nothing persisted");

        let mut m = input("x");
        m.managed = "cloud".into();
        assert!(profile_upsert(&mut doc, m).is_err());
        assert!(profile_upsert(&mut doc, input("")).is_err());
        assert!(doc.is_empty());
    }

    #[test]
    fn active_record_resolves_the_active_name_against_stored_records() {
        let mut doc = ProfilesDoc::new();
        profile_upsert(&mut doc, input("local")).unwrap();
        profile_upsert(&mut doc, input("remote")).unwrap();

        let rec = resolve_active(&doc, Some("remote")).unwrap();
        assert_eq!(rec.name, "remote");
        assert_eq!(rec.base_url, "http://127.0.0.1");
        assert!(!rec.has_secret);

        // no active name / stale name pointing at a deleted record
        assert!(resolve_active(&doc, None).is_none());
        assert!(resolve_active(&doc, Some("ghost")).is_none());

        // corrupt record value degrades to None rather than panicking
        let mut corrupt = doc.clone();
        corrupt.insert("bad".into(), serde_json::Value::Bool(true));
        assert!(resolve_active(&corrupt, Some("bad")).is_none());
    }

    #[test]
    fn deleting_the_active_profile_clears_active() {
        let mut doc = ProfilesDoc::new();
        profile_upsert(&mut doc, input("local")).unwrap();
        profile_upsert(&mut doc, input("remote")).unwrap();
        let mut active = Some("local".to_string());

        delete_profile(&mut doc, &mut active, "local");
        assert!(active.is_none());
        assert!(profile_records(&doc).iter().all(|r| r.name != "local"));
    }

    #[test]
    fn deleting_a_non_active_profile_leaves_active_untouched() {
        let mut doc = ProfilesDoc::new();
        profile_upsert(&mut doc, input("local")).unwrap();
        profile_upsert(&mut doc, input("remote")).unwrap();
        let mut active = Some("local".to_string());

        delete_profile(&mut doc, &mut active, "remote");
        assert_eq!(active.as_deref(), Some("local"));
    }

    #[test]
    fn profiles_doc_parsing_tolerates_absent_and_non_object_values() {
        assert!(doc_from_value(None).is_empty());
        assert!(doc_from_value(Some(&serde_json::Value::Null)).is_empty());
        assert!(doc_from_value(Some(&serde_json::json!("nope"))).is_empty());

        let mut doc = ProfilesDoc::new();
        profile_upsert(&mut doc, input("local")).unwrap();
        let v = serde_json::Value::Object(doc.clone());
        assert_eq!(profile_records(&doc_from_value(Some(&v))), profile_records(&doc));
    }

    /// A headless app with the real store plugin, pointed at an isolated XDG
    /// data dir so tests never touch actual user data.
    /// ponytail: mutates process-global env from a test thread, which is UB
    /// against a concurrent `getenv` (the server tests read HOME/PATH/SHELL).
    /// Bounded to one mutation via `LazyLock` and done before any store call.
    /// The clean upgrade is a `tests/store.rs` of its own — which needs this
    /// binary crate split into lib + bin, more surgery than the risk is worth.
    static STORE_DIR: std::sync::LazyLock<std::path::PathBuf> = std::sync::LazyLock::new(|| {
        let dir = std::env::temp_dir().join(format!("cdash-tauri-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("XDG_DATA_HOME", &dir);
        dir
    });

    fn mock_app_with_store() -> tauri::App<tauri::test::MockRuntime> {
        std::sync::LazyLock::force(&STORE_DIR);
        let app = tauri::test::mock_app();
        app.handle()
            .plugin(tauri_plugin_store::Builder::new().build())
            .expect("plugin registers on the mock app");
        app
    }

    #[test]
    fn profile_commands_round_trip_through_the_real_store() {
        let app = mock_app_with_store();
        let handle = app.handle().clone();

        profile_save(handle.clone(), input("local")).unwrap();
        profile_save(handle.clone(), input("remote")).unwrap();
        profile_activate(handle.clone(), "remote".into()).unwrap();
        assert!(profile_activate(handle.clone(), "ghost".into()).is_err());

        let listed = profiles_list(handle.clone()).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "local"); // sorted

        // a fresh store instance re-reads what was flushed to disk
        let reread = tauri_plugin_store::StoreBuilder::new(&handle, "profiles.json")
            .build()
            .expect("store builds");
        reread.reload().expect("saved file parses");
        assert_eq!(reread.get("active").and_then(|v| v.as_str().map(str::to_string)), Some("remote".into()));
        assert_eq!(profiles_doc(&reread).len(), 2);

        // deleting the active profile persists the cleared "active" key
        profile_delete(handle.clone(), "remote".into()).unwrap();
        let store = open_store(&handle).unwrap();
        assert_eq!(profiles_doc(&store).len(), 1);
        assert_eq!(store.get("active"), Some(serde_json::Value::Null));
        assert_eq!(profiles_list(handle).unwrap()[0].name, "local");

        let _ = std::fs::remove_dir_all(&*STORE_DIR);
    }
}

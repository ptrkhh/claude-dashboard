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

/// What a thin client says instead of starting a server. Everything the agent
/// drives — tmux, `claude`, `/proc` — lives in the WSL distro or in Termux;
/// `host_platform` tells the UI which setup instructions to show.
#[cfg(not(inproc_agent))]
const NO_AGENT_HINT: &str = "this build has no in-process agent: run cdash-agent \
    in WSL or Termux on port 8080, and the client will reach it at \
    http://localhost:8080";

/// Where a thin client looks when no profile names another agent. WSL2 relays a
/// loopback listener in the distro to the Windows host on the same port, and
/// Android does not isolate loopback between apps, so on both the agent's own
/// port is simply reachable.
const LOCAL_AGENT_BASE: &str = "http://localhost:8080";

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

#[cfg(not(inproc_agent))]
fn start_locked(_state: &Mutex<Option<Bound>>) -> Result<String, String> {
    Err(NO_AGENT_HINT.into())
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
        .or_else(|| (!cfg!(inproc_agent)).then(|| LOCAL_AGENT_BASE.to_string()))
}

/// The entire data path: JS names a path, we resolve it against the bound
/// loopback address only. No cookie jar; the client is built once.
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
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;

    /// The `aarch64-unknown-linux-musl` agent, embedded when `CDASH_AGENT_BIN`
    /// named one at build time; empty otherwise.
    pub const AGENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cdash-agent.bin"));

    static URL: Mutex<Option<String>> = Mutex::new(None);

    pub fn start() -> Result<String, String> {
        start_serving(AGENT)
    }

    /// Idempotent: the second call returns the first listener's URL rather than
    /// binding a second port.
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
                let _ = respond(conn, body);
            }
        });
        let addr = format!("http://127.0.0.1:{port}/cdash-agent");
        *url = Some(addr.clone());
        Ok(addr)
    }

    /// The request is read and discarded: one file, one response, whatever was
    /// asked for. Reading it at all is what keeps the client from seeing a
    /// reset instead of the body.
    fn respond(mut conn: TcpStream, body: &[u8]) -> std::io::Result<()> {
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

/// The URL Termux should `curl`, and the byte count so the UI can show it.
#[tauri::command]
fn agent_handoff() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({ "url": handoff::start()?, "bytes": handoff::AGENT.len() }))
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
        .manage(ServerState::default())
        // No redirects: this client has exactly one destination, and a
        // redirect is the only way a request pinned to loopback could leave it.
        .manage(ReqwestState(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
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
            host_platform
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn boot() -> (Mutex<Option<Bound>>, String) {
        let state = Mutex::new(None);
        let addr = std::thread::scope(|s| s.spawn(|| start_locked(&state).unwrap()).join().unwrap());
        (state, addr)
    }

    fn shutdown(state: &Mutex<Option<Bound>>) {
        std::thread::scope(|s| s.spawn(|| stop_locked(state).unwrap()).join().unwrap());
    }

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
        let (state, addr) = boot();
        let http = reqwest::Client::new();
        let err = request_inner(&http, Some(addr), None, "GET".into(), "api/x".into(), None)
            .await
            .unwrap_err();
        assert!(err.contains("absolute"), "got: {err}");
        shutdown(&state);
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
    static STORE_DIR: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
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

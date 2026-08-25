#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cdash_agent::auth::config::{AuthConfig, GuardKind};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Running {
    addr: String,
    bound: cdash_agent::http::serve::Bound,
}

#[derive(Default)]
pub struct ServerState(Mutex<Option<Running>>);

/// The in-process trust shape: loopback only, no auth guard, ephemeral port.
/// Mirrors the agent crate's own test config.
fn server_config() -> Result<cdash_agent::http::serve::Config, String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    Ok(cdash_agent::http::serve::Config {
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
    })
}

fn start_locked(state: &Mutex<Option<Running>>) -> Result<String, String> {
    let mut guard = state.lock().map_err(|_| "server state poisoned")?;
    if let Some(r) = guard.as_ref() {
        return Ok(r.addr.clone()); // idempotent
    }
    let cfg = server_config()?;
    let b = tauri::async_runtime::block_on(cdash_agent::http::serve::serve(cfg))
        .map_err(|e| format!("cannot start agent server: {e}"))?;
    let addr = format!("http://{}", b.addr);
    *guard = Some(Running { addr: addr.clone(), bound: b });
    Ok(addr)
}

fn stop_locked(state: &Mutex<Option<Running>>) {
    let running = state.lock().ok().and_then(|mut g| g.take());
    if let Some(r) = running {
        tauri::async_runtime::block_on(r.bound.stop());
    }
}

fn addr_locked(state: &Mutex<Option<Running>>) -> Option<String> {
    state.lock().ok().and_then(|g| g.as_ref().map(|r| r.addr.clone()))
}

#[tauri::command]
fn server_start(state: tauri::State<ServerState>) -> Result<String, String> {
    start_locked(&state.0)
}

#[tauri::command]
fn server_stop(state: tauri::State<ServerState>) {
    stop_locked(&state.0)
}

#[tauri::command]
fn server_state(state: tauri::State<ServerState>) -> Option<String> {
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

fn record_of(input: &ProfileInput) -> ProfileRecord {
    ProfileRecord {
        name: input.name.clone(),
        base_url: input.base_url.clone(),
        managed: input.managed.clone(),
        auth: input.auth.clone(),
        has_secret: false, // step 10 wires the keyring
    }
}

fn profile_upsert(profiles: &mut ProfilesDoc, input: ProfileInput) -> Result<(), String> {
    validate_profile(&input)?;
    let rec = record_of(&input);
    profiles.insert(rec.name.clone(), serde_json::to_value(&rec).expect("serializable"));
    Ok(())
}

fn profile_remove(profiles: &mut ProfilesDoc, name: &str) {
    profiles.remove(name);
}

/// The delete command's whole logic: removing the active profile clears
/// "active"; anything else leaves it untouched.
fn delete_profile(profiles: &mut ProfilesDoc, active: &mut Option<String>, name: &str) {
    if active.as_deref() == Some(name) {
        *active = None;
    }
    profile_remove(profiles, name);
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

fn open_store(app: &tauri::AppHandle) -> Result<Arc<tauri_plugin_store::Store<tauri::Wry>>, String> {
    use tauri_plugin_store::StoreExt;
    app.store("profiles.json").map_err(|e| format!("store unavailable: {e}"))
}

fn profiles_doc(store: &tauri_plugin_store::Store<tauri::Wry>) -> ProfilesDoc {
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

fn active_record(store: &tauri_plugin_store::Store<tauri::Wry>) -> Option<ProfileRecord> {
    let active = store.get("active")?.as_str()?.to_string();
    resolve_active(&profiles_doc(store), Some(&active))
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
    let m = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        m => reqwest::Method::from_bytes(m.as_bytes()).map_err(|e| e.to_string())?,
    };
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
    let addr = state.0.lock().map_err(|_| "server state poisoned")?.as_ref().map(|r| r.addr.clone());
    let active = open_store(&app).ok().and_then(|s| active_record(&s));
    request_inner(&http.0, addr, active.as_ref(), method, path, body).await
}

#[tauri::command]
fn profiles_list(app: tauri::AppHandle) -> Result<Vec<ProfileRecord>, String> {
    let store = open_store(&app)?;
    Ok(profile_records(&profiles_doc(&store)))
}

#[tauri::command]
fn profile_save(app: tauri::AppHandle, profile: ProfileInput) -> Result<(), String> {
    let store = open_store(&app)?;
    let mut doc = profiles_doc(&store);
    profile_upsert(&mut doc, profile)?;
    store.set("profiles", serde_json::Value::Object(doc));
    store.save().map_err(|e| format!("cannot persist profile: {e}"))
}

#[tauri::command]
fn profile_delete(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let store = open_store(&app)?;
    let mut doc = profiles_doc(&store);
    let mut active = store.get("active").and_then(|v| v.as_str().map(str::to_string));
    delete_profile(&mut doc, &mut active, &name);
    store.set("profiles", serde_json::Value::Object(doc));
    store.set("active", active.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null));
    store.save().map_err(|e| format!("cannot persist profiles: {e}"))
}

#[tauri::command]
fn profile_activate(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let store = open_store(&app)?;
    if !profiles_doc(&store).contains_key(&name) {
        return Err(format!("unknown profile {name:?}"));
    }
    store.set("active", name);
    store.save().map_err(|e| format!("cannot persist active profile: {e}"))
}

#[tauri::command]
fn host_platform() -> String {
    std::env::consts::OS.to_string()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(ServerState::default())
        .manage(ReqwestState(reqwest::Client::new()))
        .invoke_handler(tauri::generate_handler![
            server_start,
            server_stop,
            server_state,
            api_request,
            profiles_list,
            profile_save,
            profile_delete,
            profile_activate,
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
        assert_eq!(addr_locked(&state), Some(first));
        stop_locked(&state);
        assert_eq!(addr_locked(&state), None, "stop must clear the state");
    }

    #[test]
    fn stop_then_start_binds_a_fresh_port() {
        let state = Mutex::new(None);
        let first = start_locked(&state).unwrap();
        stop_locked(&state);
        let second = start_locked(&state).unwrap();
        assert_ne!(first, second, "ephemeral ports after teardown are re-drawn");
        stop_locked(&state);
    }

    #[test]
    fn state_of_a_stopped_server_is_none() {
        let state = Mutex::new(None);
        assert_eq!(addr_locked(&state), None);
        start_locked(&state).unwrap();
        stop_locked(&state);
        assert_eq!(addr_locked(&state), None);
    }

    /// `start_locked`/`stop_locked` block on tauri's own runtime; they must
    /// not run inside this test's tokio context.
    fn boot() -> (Mutex<Option<Running>>, String) {
        let state = Mutex::new(None);
        let addr = std::thread::scope(|s| s.spawn(|| start_locked(&state).unwrap()).join().unwrap());
        (state, addr)
    }

    fn shutdown(state: &Mutex<Option<Running>>) {
        std::thread::scope(|s| s.spawn(|| stop_locked(state)).join().unwrap());
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

        profile_remove(&mut doc, "remote");
        assert_eq!(profile_records(&doc).len(), 1);
        profile_remove(&mut doc, "missing"); // delete of unknown is a no-op
        assert_eq!(profile_records(&doc).len(), 1);
    }

    #[test]
    fn profiles_survive_a_json_round_trip() {
        let mut doc = ProfilesDoc::new();
        profile_upsert(&mut doc, input("local")).unwrap();
        let stored = serde_json::to_value(doc.clone()).unwrap();
        let restored: ProfilesDoc = serde_json::from_value(stored).unwrap();
        assert_eq!(profile_records(&restored), profile_records(&doc));
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
}

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

/// The entire data path: JS names a path, we resolve it against the bound
/// loopback address only. No cookie jar; the client is built once.
async fn request_inner(
    http: &reqwest::Client,
    addr: Option<String>,
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
    let req = http.request(m, &url);
    let req = if let Some(b) = body { req.json(&b) } else { req };
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let body = res.json::<serde_json::Value>().await.unwrap_or(serde_json::Value::Null);
    Ok(ApiResponse { status, body })
}

#[tauri::command]
async fn api_request(
    state: tauri::State<'_, ServerState>,
    http: tauri::State<'_, ReqwestState>,
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<ApiResponse, String> {
    let addr = state.0.lock().map_err(|_| "server state poisoned")?.as_ref().map(|r| r.addr.clone());
    request_inner(&http.0, addr, method, path, body).await
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
            api_request
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
        let res = request_inner(&http, Some(addr), "GET".into(), "/api/health".into(), None)
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
        let err = request_inner(&http, Some(addr), "GET".into(), "api/x".into(), None)
            .await
            .unwrap_err();
        assert!(err.contains("absolute"), "got: {err}");
        shutdown(&state);
    }

    #[tokio::test]
    async fn requests_without_a_running_server_fail() {
        let http = reqwest::Client::new();
        let err = request_inner(&http, None, "GET".into(), "/api/health".into(), None)
            .await
            .unwrap_err();
        assert_eq!(err, "server not running");
    }
}

use crate::collect::browse::list_dirs;
use crate::collect::ctx::Ctx;
use crate::collect::places::read_places;
use crate::collect::sessions::collect_sessions;
use crate::collect::validate::{BadRequest, Refused};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::collections::HashMap;
use std::sync::Arc;

/// Mirrors the error shape of `server.js:41`: a status and `{ error: message }`.
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

impl From<BadRequest> for ApiError {
    fn from(e: BadRequest) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: e.0 }
    }
}

impl From<Refused> for ApiError {
    fn from(e: Refused) -> Self {
        match e {
            Refused::BadRequest(m) => Self { status: StatusCode::BAD_REQUEST, message: m },
            Refused::Failed(m) => Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: m },
        }
    }
}

/// `/api/sessions` is the one route that answers 500 rather than 400 on
/// failure, and logs first (`server.js:34`). In practice it cannot fail: every
/// fallible step inside `collect_sessions` already degrades to a default.
pub async fn get_sessions(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(collect_sessions(&ctx).await).into_response()
}

/// Authenticated: it names the host's platform and which binaries are absent.
/// `/api/health` is the unauthenticated one and says only `{ok:true}`.
pub async fn get_hostinfo(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(serde_json::json!({
        "platform": std::env::consts::OS,
        "version": env!("CARGO_PKG_VERSION"),
        // Re-probed per request, never a boot-time cache: the setup screen's
        // re-check button is worthless against a stale answer.
        "missing": ctx.host.missing(),
    }))
    .into_response()
}

pub async fn get_logs(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(serde_json::json!({ "lines": ctx.host.log.lines() })).into_response()
}

pub async fn get_places(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(read_places(&ctx.places_file).await).into_response()
}

pub async fn get_browse(Query(q): Query<HashMap<String, String>>) -> Result<Response, ApiError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    let target = q.get("path").filter(|p| !p.is_empty()).cloned().unwrap_or(home);
    let hidden = q.get("hidden").map(|h| h == "1").unwrap_or(false);
    Ok(Json(list_dirs(&target, hidden).await?).into_response())
}

use crate::collect::places::{add_recent, toggle_favorite};
use crate::collect::keys::send_keys;
use crate::collect::spawn::{kill_session, launch_session, purge_session, resume_session};
use crate::collect::validate::assert_path;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct PathBody {
    #[serde(default)]
    pub path: String,
}

#[derive(Deserialize)]
pub struct LaunchBody {
    #[serde(default)]
    pub dir: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_effort")]
    pub effort: String,
}

// Node destructured `{ dir, model = 'sonnet', effort = 'medium' }`
// (`lib/collect.js:159`); an absent field must not become a rejected request.
fn default_model() -> String {
    "sonnet".to_string()
}
fn default_effort() -> String {
    "medium".to_string()
}

#[derive(Deserialize)]
pub struct SidBody {
    #[serde(default)]
    pub sid: String,
}

#[derive(Deserialize)]
pub struct NameBody {
    #[serde(default)]
    pub name: String,
}

pub async fn post_favorites(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<PathBody>,
) -> Result<Response, ApiError> {
    assert_path(&body.path)?;
    let places = toggle_favorite(&ctx.places_file, &body.path).await.map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    Ok(Json(places).into_response())
}

pub async fn post_launch(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<LaunchBody>,
) -> Result<Response, ApiError> {
    let name = launch_session(&ctx, &body.dir, &body.model, &body.effort).await?;

    // Fire-and-forget, exactly as `server.js:56`: a failed recents write logs
    // and does not fail the launch. The route resolves the directory before
    // recording it; `launch_session` received the raw value.
    let places_file = ctx.places_file.clone();
    let resolved = std::path::absolute(&body.dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(&body.dir))
        .to_string_lossy()
        .into_owned();
    let log = Arc::clone(&ctx.host.log);
    tokio::spawn(async move {
        if let Err(e) = add_recent(&places_file, &resolved).await {
            log.push(format!("recent write failed: {e}"));
        }
    });

    Ok(Json(serde_json::json!({ "name": name })).into_response())
}

pub async fn post_resume(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<SidBody>,
) -> Result<Response, ApiError> {
    let name = resume_session(&ctx, &body.sid).await?;
    Ok(Json(serde_json::json!({ "name": name })).into_response())
}

pub async fn post_kill(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<NameBody>,
) -> Result<Response, ApiError> {
    kill_session(&ctx, &body.name).await?;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

#[derive(Deserialize)]
pub struct KeysBody {
    #[serde(default)]
    pub name: String,
    /// Deliberately not defaulted: an absent field is "text required", not an
    /// empty keystroke sent to a live pane.
    pub text: Option<String>,
}

/// Type into a session's TUI. Stopgap for the Claude app's remote control,
/// which can send prompts but can't answer a session that asks you to run
/// something interactively (`! gcloud auth login`).
pub async fn post_keys(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<KeysBody>,
) -> Result<Response, ApiError> {
    send_keys(&ctx, &body.name, body.text.as_deref()).await?;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

pub async fn post_purge(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<SidBody>,
) -> Result<Response, ApiError> {
    purge_session(&ctx, &body.sid)?;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

/// Behind the guard: revoking a session requires holding one.
///
/// The unused `Json` extractor is the CSRF control, not decoration: it is what
/// answers 415 to the three CORS-simple content types, so a sibling origin's
/// auto-submitting form cannot revoke a live session. Callers send `{}`.
pub async fn post_logout(
    State(ctx): State<Arc<Ctx>>,
    headers: axum::http::HeaderMap,
    Json(_body): Json<serde_json::Value>,
) -> Response {
    match ctx.password.get() {
        Some(pw) => crate::auth::login::post_logout(pw, &headers).await,
        // Reachable only under a chain without `password`, where there is no
        // session to revoke.
        None => Json(serde_json::json!({ "ok": true })).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::http::serve::serve;
    use crate::http::serve::tests::{cfg_for, http_get, http_post, reqwest_get};

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-routes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(dir.join("history.jsonl"), "").unwrap();
        dir
    }

    #[tokio::test]
    async fn sessions_returns_the_three_top_level_keys() {
        let b = serve(cfg_for(tempdir("sessions"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/sessions", b.addr)).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["running"].is_array());
        assert!(v["resumable"].is_array());
        assert!(v["stats"]["ramTotalKb"].is_number());
    }

    #[tokio::test]
    async fn hostinfo_reports_platform_version_and_missing_binaries() {
        let b = serve(cfg_for(tempdir("hostinfo"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/hostinfo", b.addr)).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["platform"], std::env::consts::OS);
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert!(v["missing"].is_array(), "the setup screen reads this array");
    }

    #[tokio::test]
    async fn hostinfo_missing_is_recomputed_not_a_boot_time_cache() {
        // UX-5: a user who installs tmux while the agent runs and presses
        // re-check must get the new answer.
        let b = serve(cfg_for(tempdir("hostinfo-recheck"))).await.unwrap();
        let url = format!("http://{}/api/hostinfo", b.addr);
        assert_eq!(reqwest_get(&url).await, reqwest_get(&url).await);
    }

    #[tokio::test]
    async fn logs_returns_a_lines_array() {
        let b = serve(cfg_for(tempdir("logs"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/logs", b.addr)).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["lines"].is_array());
    }

    #[tokio::test]
    async fn places_returns_the_empty_shape_when_no_file_exists() {
        let b = serve(cfg_for(tempdir("places"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/places", b.addr)).await;
        assert_eq!(body, "{\"recents\":[],\"favorites\":[]}");
    }

    #[tokio::test]
    async fn browse_lists_a_directory_and_defaults_to_home() {
        let d = tempdir("browse");
        std::fs::create_dir_all(d.join("child")).unwrap();
        let b = serve(cfg_for(d.clone())).await.unwrap();

        let body = reqwest_get(&format!(
            "http://{}/api/browse?path={}",
            b.addr,
            d.to_str().unwrap()
        ))
        .await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["entries"].as_array().unwrap().iter().any(|e| e["name"] == "child"));

        // No `path` at all must still answer — Node defaulted to the home dir.
        let (status, _) = http_get(&format!("http://{}/api/browse", b.addr)).await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn browse_hidden_is_opt_in_via_the_query_string() {
        let d = tempdir("browse-hidden");
        std::fs::create_dir_all(d.join(".secret")).unwrap();
        let b = serve(cfg_for(d.clone())).await.unwrap();
        let p = d.to_str().unwrap();

        let plain = reqwest_get(&format!("http://{}/api/browse?path={p}", b.addr)).await;
        assert!(!plain.contains(".secret"));
        let shown = reqwest_get(&format!("http://{}/api/browse?path={p}&hidden=1", b.addr)).await;
        assert!(shown.contains(".secret"));
    }

    #[tokio::test]
    async fn a_browse_failure_is_a_400_naming_the_problem_not_a_500() {
        let b = serve(cfg_for(tempdir("browse-fail"))).await.unwrap();
        let (status, body) =
            http_get(&format!("http://{}/api/browse?path=/no/such/cdash-dir", b.addr)).await;
        assert_eq!(status, 400);
        assert_eq!(body, "{\"error\":\"No such folder\"}");
    }

    #[tokio::test]
    async fn favorites_rejects_a_relative_path_before_writing_anything() {
        // A1 reaching the route. The validator has its own test; this proves
        // the route calls it.
        let d = tempdir("fav-guard");
        let b = serve(cfg_for(d.clone())).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/favorites", b.addr),
            "{\"path\":\"relative/x\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad path"));
        assert!(!d.join("cdash-places.json").exists(), "nothing may be written");
    }

    #[tokio::test]
    async fn favorites_toggles_and_persists() {
        let d = tempdir("fav-ok");
        let b = serve(cfg_for(d.clone())).await.unwrap();
        let url = format!("http://{}/api/favorites", b.addr);
        let (status, body) = http_post(&url, "{\"path\":\"/home/x/proj\"}").await;
        assert_eq!(status, 200);
        assert!(body.contains("/home/x/proj"));

        let (_, body) = http_post(&url, "{\"path\":\"/home/x/proj\"}").await;
        assert_eq!(body, "{\"recents\":[],\"favorites\":[]}", "second call toggles off");
    }

    #[tokio::test]
    async fn launch_rejects_a_model_outside_the_allowlist() {
        let b = serve(cfg_for(tempdir("launch-guard"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/launch", b.addr),
            "{\"dir\":\"/tmp\",\"model\":\"gpt-4\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad model"));
    }

    #[tokio::test]
    async fn launch_rejects_a_directory_that_is_not_one() {
        let b = serve(cfg_for(tempdir("launch-dir"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/launch", b.addr),
            "{\"dir\":\"/no/such/cdash-dir\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("not a directory"));
    }

    #[tokio::test]
    async fn resume_rejects_a_sid_that_is_not_a_uuid() {
        let b = serve(cfg_for(tempdir("resume-guard"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/resume", b.addr),
            "{\"sid\":\"not-a-uuid; rm -rf /\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad sid"));
    }

    #[tokio::test]
    async fn kill_rejects_a_name_that_is_not_a_cdash_session() {
        let b = serve(cfg_for(tempdir("kill-guard"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/kill", b.addr),
            "{\"name\":\"other; rm -rf /\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad name"));
    }

    #[tokio::test]
    async fn keys_rejects_a_name_that_is_not_a_cdash_session() {
        // The value reaches `tmux send-keys -t`, so it answers to the same
        // allowlist `/api/kill` does.
        let b = serve(cfg_for(tempdir("keys-guard"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/keys", b.addr),
            "{\"name\":\"other; rm -rf /\",\"text\":\"hi\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad name"));
    }

    #[tokio::test]
    async fn keys_rejects_an_absent_or_empty_text_before_touching_tmux() {
        let b = serve(cfg_for(tempdir("keys-text"))).await.unwrap();
        let url = format!("http://{}/api/keys", b.addr);

        let (status, body) = http_post(&url, "{\"name\":\"cdash-a\"}").await;
        assert_eq!(status, 400, "an absent text must not become a bare Enter");
        assert!(body.contains("text required"));

        let (status, body) = http_post(&url, "{\"name\":\"cdash-a\",\"text\":\"  \"}").await;
        assert_eq!(status, 400);
        assert!(body.contains("empty text"));
    }

    #[tokio::test]
    async fn a_failed_send_is_a_500_not_a_cheerful_ok() {
        // No tmux server is running under test, so the send must fail — and
        // reporting a keystroke that never reached the pane is the defect this
        // guards. `Refused::Failed` is what makes it a 500 rather than `{ok:true}`.
        let b = serve(cfg_for(tempdir("keys-fail"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/keys", b.addr),
            "{\"name\":\"cdash-no-such-session\",\"text\":\"hello\"}",
        )
        .await;
        assert_eq!(status, 500);
        assert!(body.contains("tmux send-keys"), "the error names the failing command: {body}");
    }

    #[tokio::test]
    async fn purge_guards_the_sid_and_then_hides_it() {
        let b = serve(cfg_for(tempdir("purge"))).await.unwrap();
        let url = format!("http://{}/api/purge", b.addr);

        let (status, _) = http_post(&url, "{\"sid\":\"../../etc/passwd\"}").await;
        assert_eq!(status, 400);

        let sid = "2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34";
        let (status, body) = http_post(&url, &format!("{{\"sid\":\"{sid}\"}}")).await;
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
        assert!(b.ctx.purged.lock().unwrap().contains(sid));
    }

    #[tokio::test]
    async fn static_files_are_served_from_the_configured_directory() {
        let d = tempdir("static");
        let pubdir = d.join("pub");
        std::fs::create_dir_all(&pubdir).unwrap();
        std::fs::write(pubdir.join("index.html"), "<h1>cdash</h1>").unwrap();

        let mut cfg = cfg_for(d.clone());
        cfg.public_dir = pubdir;
        let b = serve(cfg).await.unwrap();

        let body = reqwest_get(&format!("http://{}/index.html", b.addr)).await;
        assert_eq!(body, "<h1>cdash</h1>");
    }

    #[tokio::test]
    async fn an_api_route_is_not_shadowed_by_a_static_file_of_the_same_name() {
        // Express mounted static FIRST, so this file would have won there. The
        // route winning is the safer order; asserted so the change is deliberate.
        let d = tempdir("shadow");
        let pubdir = d.join("pub");
        std::fs::create_dir_all(pubdir.join("api")).unwrap();
        std::fs::write(pubdir.join("api/health"), "STATIC").unwrap();

        let mut cfg = cfg_for(d.clone());
        cfg.public_dir = pubdir;
        let b = serve(cfg).await.unwrap();

        let body = reqwest_get(&format!("http://{}/api/health", b.addr)).await;
        assert_eq!(body, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn a_missing_body_field_is_rejected_rather_than_defaulted_into_a_subprocess() {
        let b = serve(cfg_for(tempdir("missing"))).await.unwrap();
        let (status, _) = http_post(&format!("http://{}/api/kill", b.addr), "{}").await;
        assert_eq!(status, 400, "an absent name must not become an empty tmux target");
    }
}

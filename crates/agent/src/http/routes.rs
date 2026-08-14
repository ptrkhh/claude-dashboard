use crate::collect::browse::{list_dirs, BrowseError};
use crate::collect::ctx::Ctx;
use crate::collect::places::read_places;
use crate::collect::sessions::collect_sessions;
use crate::collect::validate::BadRequest;
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

impl From<BrowseError> for ApiError {
    fn from(e: BrowseError) -> Self {
        Self {
            status: StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_REQUEST),
            message: e.message,
        }
    }
}

/// `/api/sessions` is the one route that answers 500 rather than 400 on
/// failure, and logs first (`server.js:34`). In practice it cannot fail: every
/// fallible step inside `collect_sessions` already degrades to a default.
pub async fn get_sessions(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(collect_sessions(&ctx).await).into_response()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::serve::serve;
    use crate::http::serve::tests::{cfg_for, http_get, reqwest_get};

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
}

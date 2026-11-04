//! Guarded OpenCode request validation without launching real sessions.
mod common;
use cdash_agent::auth::config::{AuthConfig, GuardKind};
use cdash_agent::http::serve::{serve, Config};
use std::sync::Arc;

#[tokio::test]
async fn opencode_routes_are_guarded_and_reject_bad_inputs_before_spawn() {
    let dir = std::env::temp_dir().join(format!("cdash-opencode-routes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sessions")).unwrap();
    let auth = AuthConfig::build(vec![GuardKind::Bearer], Some("fixture-token".into()), "X-Forwarded-Email".into(), vec![]).unwrap();
    let server = serve(Config { bind: "127.0.0.1".parse().unwrap(), port: 0, claude_dir: dir.clone(), disk_extra: None, auth: Arc::new(auth), password: None }).await.unwrap();
    let addr = server.addr.to_string();
    let unauthorized = common::send(&addr, "GET", "/api/models?dir=relative", &[], None).await;
    assert_eq!(unauthorized.status().as_u16(), 401);
    let authorized = ["Authorization: Bearer fixture-token"];
    let invalid_dir = common::send(&addr, "GET", "/api/models?dir=relative", &authorized, None).await;
    assert_eq!(invalid_dir.status().as_u16(), 400);
    let invalid_launch = common::send(&addr, "POST", "/api/launch", &authorized, Some(("application/json", r#"{"dir":"relative","model":"opencode:fixture/model","variant":"deep"}"#))).await;
    assert_eq!(invalid_launch.status().as_u16(), 400);
    let invalid_resume = common::send(&addr, "POST", "/api/resume", &authorized, Some(("application/json", r#"{"sid":"ses_../bad"}"#))).await;
    assert_eq!(invalid_resume.status().as_u16(), 400);
    let purge = common::send(&addr, "POST", "/api/purge", &authorized, Some(("application/json", r#"{"sid":"ses_saved"}"#))).await;
    assert_eq!(purge.status().as_u16(), 200);
    assert!(!dir.join("cdash-opencode").exists());
    let _ = std::fs::remove_dir_all(dir);
}

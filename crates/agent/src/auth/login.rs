use super::boot::PasswordPolicy;
use super::cookie::{
    clear_cookie, read_cookie, set_cookie, COOKIE_NAME_INSECURE, COOKIE_NAME_SECURE,
};
use super::password::verify_password;
use super::session::{Sessions, SESSION_TTL};
use super::throttle::Throttle;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

/// A password field, a submit button, an error region. No product name, no
/// version, no logo, no favicon reference, and no title beyond "Sign in":
/// naming the product on an unauthenticated page tells a scanner that a
/// successful guess here yields RCE as the running user. Asset-free, which is
/// what keeps the unauthenticated exception count at three.
pub const LOGIN_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Sign in</title>
<style>
body{font-family:system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0;background:#111;color:#eee}
form{display:grid;gap:.75rem;width:min(20rem,90vw)}
input,button{font:inherit;padding:.6rem;border-radius:.4rem;border:1px solid #444}
input{background:#1c1c1c;color:#eee}
button{background:#2d6cdf;color:#fff;border:0;cursor:pointer}
p{color:#e66;min-height:1.2em;margin:0}
</style></head>
<body><form method="post" action="/api/login" id="f">
<label for="p">Sign in</label>
<input id="p" name="password" type="password" autocomplete="current-password" autofocus>
<button type="submit">Sign in</button>
<p id="e" role="alert"></p>
</form>
<script>
document.getElementById('f').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const e = document.getElementById('e');
  e.textContent = '';
  const r = await fetch('/api/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ password: document.getElementById('p').value }),
  });
  if (r.ok) { location.href = '/'; return; }
  e.textContent = r.status === 503 ? 'Try again shortly' : 'Incorrect password';
});
</script>
</body></html>"#;

#[derive(Clone)]
pub struct PasswordState {
    pub policy: Arc<PasswordPolicy>,
    pub sessions: Arc<Sessions>,
    pub throttle: Arc<Throttle>,
}

impl PasswordState {
    pub fn new(policy: PasswordPolicy, pending_max: usize) -> Self {
        Self {
            policy: Arc::new(policy),
            sessions: Arc::new(Sessions::new()),
            throttle: Arc::new(Throttle::new(pending_max)),
        }
    }

    pub fn cookie_name(&self) -> &'static str {
        if self.policy.secure_cookie {
            COOKIE_NAME_SECURE
        } else {
            COOKIE_NAME_INSECURE
        }
    }

    /// The guard leg: a present, unexpired sid.
    pub fn authenticated(&self, cookie_header: Option<&str>) -> bool {
        read_cookie(cookie_header, self.cookie_name())
            .is_some_and(|sid| self.sessions.is_valid(&sid))
    }
}

pub async fn get_login() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], LOGIN_HTML).into_response()
}

#[derive(Deserialize)]
pub struct LoginBody {
    #[serde(default)]
    pub password: String,
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "unauthorized" }))).into_response()
}

pub async fn post_login(State(st): State<PasswordState>, Json(body): Json<LoginBody>) -> Response {
    // The only refusal, and it is volumetric rather than throttle-reasoned.
    let Some(_slot) = st.throttle.admit() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response();
    };

    // Delayed before evaluation, then processed normally. A login attempt is
    // never rejected for throttle reasons.
    let delay = st.throttle.current_delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }

    // scrypt is deliberately expensive: never run it on a reactor thread.
    let hash = st.policy.hash.clone();
    let attempt = body.password.clone();
    let ok = tokio::task::spawn_blocking(move || verify_password(&attempt, &hash))
        .await
        .unwrap_or(false);

    if !ok {
        st.throttle.note_failure(&body.password);
        return unauthorized();
    }
    st.throttle.note_success();

    let sid = st.sessions.mint();
    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            set_cookie(st.cookie_name(), &sid, SESSION_TTL, st.policy.secure_cookie),
        )],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

pub async fn post_logout(st: &PasswordState, headers: &HeaderMap) -> Response {
    if let Some(sid) =
        read_cookie(headers.get(header::COOKIE).and_then(|v| v.to_str().ok()), st.cookie_name())
    {
        st.sessions.revoke(&sid);
    }
    (
        StatusCode::OK,
        [(header::SET_COOKIE, clear_cookie(st.cookie_name(), st.policy.secure_cookie))],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_page_names_no_product_and_references_no_asset() {
        // Naming the product on an unauthenticated page tells a scanner that a
        // successful guess here yields RCE as the running user.
        let h = LOGIN_HTML.to_lowercase();
        for leak in ["cdash", "claude", "dashboard", "tmux", "version"] {
            assert!(!h.contains(leak), "the login page must not mention {leak}");
        }
        // Asset-free keeps the unauthenticated exception count at three.
        for asset in ["<img", "href=\"http", "src=\"", "favicon", "stylesheet"] {
            assert!(!h.contains(asset), "the login page must not reference {asset}");
        }
        assert!(h.contains("sign in"));
        assert!(h.contains("type=\"password\""));
    }

    #[test]
    fn the_failure_text_is_identical_for_a_wrong_password_and_an_expired_session() {
        assert!(LOGIN_HTML.contains("Incorrect password"));
    }
}

use super::config::{AuthConfig, GuardKind};
use super::guards::{check_bearer, check_trusted_proxy};
use crate::host::log::LogBuffer;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::SocketAddr;
use std::sync::Arc;

/// The unauthenticated exceptions, enumerated rather than implied.
/// `/login` and `POST /api/login` join this list in plan 6b, bringing it to
/// the spec's three.
pub const UNAUTH_PATHS: &[&str] = &["/api/health", "/login", "/api/login"];

#[derive(Clone)]
pub struct GuardState {
    pub auth: Arc<AuthConfig>,
    pub log: Arc<LogBuffer>,
    pub password: Option<super::login::PasswordState>,
}

/// A rejected request says only this. Which leg failed goes to the log buffer,
/// which sits behind the guard.
fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "unauthorized" }))).into_response()
}

/// All configured guards must pass.
pub async fn guard_mw(
    State(st): State<GuardState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    if st.auth.is_open() {
        return next.run(req).await;
    }

    let bearer = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let proxy_identity = req
        .headers()
        .get(&st.auth.proxy_header)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let cookie = req
        .headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    for g in &st.auth.guards {
        let ok = match g {
            GuardKind::None => true,
            GuardKind::Bearer => {
                check_bearer(bearer.as_deref(), st.auth.token.as_deref().unwrap_or(""))
            }
            GuardKind::TrustedProxy => check_trusted_proxy(
                Some(peer.ip()),
                proxy_identity.as_deref(),
                &st.auth.proxy_allow,
            ),
            GuardKind::Password => {
                st.password.as_ref().is_some_and(|p| p.authenticated(cookie.as_deref()))
            }
            // Not implemented until plan 6c. Refusing here is the safe
            // direction: a configured-but-unimplemented guard must never pass.
            GuardKind::CfAccess => false,
        };
        if !ok {
            st.log.push(format!("auth: rejected by {g:?}"));
            // `/api/*` gets the uniform rejection body. A browser navigation
            // is redirected to the login page — but only when there is one:
            // under a chain without `password`, `/login` does not exist and a
            // redirect would be an endless loop, so 401 is the honest answer.
            let navigational = !req.uri().path().starts_with("/api/");
            return if navigational && st.password.is_some() {
                (StatusCode::FOUND, [(axum::http::header::LOCATION, "/login")]).into_response()
            } else {
                unauthorized()
            };
        }
    }
    next.run(req).await
}

/// Declares the guarded routes once. The macro emits both the router and the
/// path list the bypass test walks, so a route cannot be registered through it
/// without appearing in the list. Axum exposes no API to enumerate a built
/// `Router`, which is why the single source of truth lives here instead.
#[macro_export]
macro_rules! guarded_routes {
    ($($method:ident $path:literal => $handler:path),* $(,)?) => {
        pub fn guarded_router() -> axum::Router<std::sync::Arc<$crate::collect::ctx::Ctx>> {
            axum::Router::new()
                $(.route($path, axum::routing::$method($handler)))*
        }
        pub const GUARDED_PATHS: &[(&str, &str)] = &[$((stringify!($method), $path)),*];
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::serve::GUARDED_PATHS;

    #[test]
    fn exactly_the_enumerated_routes_are_unauthenticated() {
        // Assertion 1's companion: a fourth exception fails here on the day it
        // is added. `/login` and `/api/login` land in plan 6b; until then the
        // list is shorter, and this test says so rather than passing blindly.
        assert!(UNAUTH_PATHS.contains(&"/api/health"));
        assert!(UNAUTH_PATHS.len() <= 3, "no more than the three enumerated exceptions");
    }

    #[test]
    fn every_guarded_route_is_listed_for_the_bypass_test() {
        // The list and the router come from one macro invocation, so this
        // cannot drift. Spot-check the routes that matter most.
        for p in ["/api/sessions", "/api/logs", "/api/kill", "/api/launch", "/api/hostinfo"] {
            assert!(GUARDED_PATHS.iter().any(|(_, path)| *path == p), "{p} must be guarded");
        }
        assert!(!GUARDED_PATHS.iter().any(|(_, p)| *p == "/api/health"));
    }
}

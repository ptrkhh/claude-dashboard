use super::config::{AuthConfig, GuardKind};
use super::guards::{check_bearer, check_trusted_proxy};
use crate::host::log::LogBuffer;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub struct GuardState {
    pub auth: Arc<AuthConfig>,
    pub log: Arc<LogBuffer>,
    pub password: Option<super::login::PasswordState>,
    pub cf: Option<Arc<super::cfaccess::CfState>>,
}

/// A rejected request says only this. Which leg failed goes to the log buffer,
/// which sits behind the guard.
fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "unauthorized" }))).into_response()
}

/// All configured guards must pass.
fn header(h: &axum::http::HeaderMap, k: &str) -> Option<String> {
    h.get(k).and_then(|v| v.to_str().ok()).map(str::to_string)
}

pub async fn guard_mw(
    State(st): State<GuardState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    if st.auth.is_open() {
        return next.run(req).await;
    }

    // `HeaderMap::get` is case-insensitive and takes `&str`, so one function
    // serves all four. A free fn and not a closure: a closure would hold its
    // borrow of `req` past the `next.run(req)` move below.
    let h = req.headers();
    let bearer = header(h, "authorization");
    let proxy_identity = header(h, &st.auth.proxy_header);
    let cf_assertion = header(h, "cf-access-jwt-assertion");
    let cookie = header(h, "cookie");

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
            GuardKind::CfAccess => st.cf.as_ref().is_some_and(|cf| {
                // No key set loaded yet means reject, not admit: a guard that
                // fails open is not a guard.
                let Some(jwks) = cf.jwks.get() else {
                    st.log.push("cf-access: no JWKS loaded");
                    return false;
                };
                cf_assertion.as_deref().is_some_and(|t| {
                    match super::cfaccess::verify_cf_jwt(t, &jwks, &cf.cfg) {
                        Ok(_) => true,
                        Err(e) => {
                            st.log.push(format!("cf-access: {e}"));
                            false
                        }
                    }
                })
            }),
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
    use crate::http::serve::GUARDED_PATHS;

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

//! Shared by both integration suites. Was three hand-rolled HTTP clients
//! parsing status lines out of a `TcpStream`; `reqwest` became a production
//! dependency when cf-access started fetching Cloudflare's JWKS, and one of
//! those parsers had already cost a debugging session by lowercasing a
//! base64url session id.

/// Header lines are raw ("Cookie: a=b") because that is the shape the
/// assertions read. `body` is `(content_type, payload)`; an empty
/// content_type sends none, which is itself a case under test.
pub async fn send(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[&str],
    body: Option<(&str, &str)>,
) -> reqwest::Response {
    // No redirects: the guard answers a navigation with 302 -> /login, and
    // following it would report 200 for a request that was refused.
    let c = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client builds");
    let m = reqwest::Method::from_bytes(method.as_bytes()).expect("test method is a token");
    let mut req = c.request(m, format!("http://{addr}{path}"));
    for h in headers {
        let (k, v) = h.split_once(": ").expect("header lines are 'Name: value'");
        req = req.header(k, v);
    }
    if let Some((ct, b)) = body {
        if !ct.is_empty() {
            req = req.header("content-type", ct);
        }
        req = req.body(b.to_string());
    }
    req.send().await.expect("request reaches the test server")
}

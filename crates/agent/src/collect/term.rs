//! A live terminal on a dashboard session: `tmux attach` in a PTY, bridged to
//! a WebSocket. This half is the request validation; the bridge follows.

use super::validate::BadRequest;
use serde::Deserialize;

/// Upper bound for either dimension. Far past any real screen, low enough
/// that a hostile value cannot ask tmux for an absurd allocation.
const MAX_DIM: u16 = 1000;

/// Whether a WebSocket upgrade carrying `origin` may attach.
///
/// A WebSocket upgrade is not subject to CORS, so without this any page open
/// on the phone could reach `ws://localhost:<port>` and type into a session,
/// which amounts to a shell. Only host[:port] is compared, never the scheme:
/// behind a TLS-terminating proxy the agent sees plain HTTP while the browser
/// sends `https://`, and a page must already be served from the agent's own
/// host to pass either way. `public_url` (`CDASH_PUBLIC_URL`) covers a proxy
/// that rewrites `Host`. Does not stop DNS rebinding; see the spec.
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>, public_url: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    if origin == "http://tauri.localhost" || origin == "tauri://localhost" {
        return true;
    }
    let Some(o) = authority(origin) else {
        return false;
    };
    host.is_some_and(|h| h.eq_ignore_ascii_case(o))
        || public_url
            .and_then(authority)
            .is_some_and(|p| p.eq_ignore_ascii_case(o))
}

/// `host[:port]` of an absolute URL: what follows `://`, up to the first `/`.
fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let a = rest.split('/').next().unwrap_or_default();
    (!a.is_empty()).then_some(a)
}

fn dim(v: Option<&str>) -> Option<u16> {
    v?.parse::<u16>().ok().filter(|n| (1..=MAX_DIM).contains(n))
}

/// The initial `cols`/`rows` query values. Required, so the first frame is
/// already drawn at phone width rather than reflowed from 80x24.
pub fn parse_size(cols: Option<&str>, rows: Option<&str>) -> Result<(u16, u16), BadRequest> {
    match (dim(cols), dim(rows)) {
        (Some(c), Some(r)) => Ok((c, r)),
        _ => Err(BadRequest(format!(
            "bad size: cols and rows must be 1..={MAX_DIM}"
        ))),
    }
}

#[derive(Deserialize)]
struct Resize {
    resize: [u16; 2],
}

/// `{"resize":[cols,rows]}`, the only text frame the browser sends. Anything
/// else is `None` and the bridge ignores it.
pub fn parse_resize(text: &str) -> Option<(u16, u16)> {
    let [c, r] = serde_json::from_str::<Resize>(text).ok()?.resize;
    let ok = |n: u16| (1..=MAX_DIM).contains(&n);
    (ok(c) && ok(r)).then_some((c, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_allows_what_only_the_dashboard_itself_can_send() {
        // Non-browser clients send no Origin and are not a cross-site vector.
        assert!(origin_allowed(None, Some("localhost:23274"), None));
        assert!(origin_allowed(
            Some("http://localhost:23274"),
            Some("localhost:23274"),
            None
        ));
        assert!(origin_allowed(
            Some("http://LOCALHOST:23274"),
            Some("localhost:23274"),
            None
        ));
        // Behind a TLS-terminating proxy the agent sees plain HTTP while the
        // browser sends https; the scheme must not matter.
        assert!(origin_allowed(
            Some("https://dash.example.com"),
            Some("dash.example.com"),
            None
        ));
        // A proxy that rewrites Host is covered by CDASH_PUBLIC_URL.
        assert!(origin_allowed(
            Some("https://dash.example.com"),
            Some("127.0.0.1:23274"),
            Some("https://dash.example.com/"),
        ));
        assert!(origin_allowed(
            Some("http://tauri.localhost"),
            Some("localhost:23274"),
            None
        ));
        assert!(origin_allowed(
            Some("tauri://localhost"),
            Some("localhost:23274"),
            None
        ));
    }

    #[test]
    fn origin_refuses_every_other_page() {
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("localhost:23274"),
            None
        ));
        // Same host, different port: a different origin, possibly another app.
        assert!(!origin_allowed(
            Some("http://localhost:3000"),
            Some("localhost:23274"),
            None
        ));
        // Sandboxed iframes and file:// pages send the literal "null".
        assert!(!origin_allowed(Some("null"), Some("localhost:23274"), None));
        assert!(!origin_allowed(Some("http://localhost:23274"), None, None));
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("127.0.0.1:23274"),
            Some("https://dash.example.com"),
        ));
        // Lookalikes of the Tauri origins are not Tauri.
        assert!(!origin_allowed(
            Some("http://tauri.localhost.evil.example"),
            Some("x"),
            None
        ));
    }

    #[test]
    fn size_must_be_two_integers_in_range() {
        assert_eq!(parse_size(Some("80"), Some("24")).unwrap(), (80, 24));
        assert_eq!(parse_size(Some("1"), Some("1000")).unwrap(), (1, 1000));
        for (c, r) in [
            (None, Some("24")),
            (Some("80"), None),
            (Some("0"), Some("24")),
            (Some("80"), Some("1001")),
            (Some("-1"), Some("24")),
            (Some("eighty"), Some("24")),
        ] {
            let e = parse_size(c, r).unwrap_err();
            assert!(e.0.contains("bad size"), "{c:?}x{r:?} gave {}", e.0);
        }
    }

    #[test]
    fn resize_message_is_the_one_text_frame_the_bridge_reads() {
        assert_eq!(parse_resize(r#"{"resize":[100,30]}"#), Some((100, 30)));
        assert_eq!(parse_resize(r#"{"resize":[0,30]}"#), None);
        assert_eq!(parse_resize(r#"{"resize":[100,5000]}"#), None);
        assert_eq!(parse_resize(r#"{"resize":[100]}"#), None);
        assert_eq!(parse_resize("hello"), None);
    }
}

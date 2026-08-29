use std::time::Duration;

/// The `__Host-` prefix is refused by browsers unless the cookie is `Secure`,
/// `Path=/` and carries no `Domain`, so a sibling subdomain cannot mint one
/// scoped to the registrable domain. Shadowing becomes structurally
/// impossible rather than merely detectable.
pub const COOKIE_NAME_SECURE: &str = "__Host-cdash_sid";
/// Used only under `CDASH_ALLOW_INSECURE_COOKIE=1`, where `Secure` is dropped
/// and the prefix must go with it.
pub const COOKIE_NAME_INSECURE: &str = "cdash_sid";

/// Last-wins on duplicates, deterministic, and never panics. This is the one
/// piece of hand-rolled parsing on attacker-influenced input; it is exempt
/// from "do not hand-roll" because it is small enough for a test to discharge.
pub fn read_cookie(header: Option<&str>, name: &str) -> Option<String> {
    let mut found = None;
    for pair in header?.split(';') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        if k.trim() == name {
            let v = v.trim();
            found = if v.is_empty() { None } else { Some(v.to_string()) };
        }
    }
    found
}

pub fn set_cookie(name: &str, sid: &str, ttl: Duration, secure: bool) -> String {
    let mut c = format!("{name}={sid}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}", ttl.as_secs());
    if secure {
        c.push_str("; Secure");
    }
    c
}

pub fn clear_cookie(name: &str, secure: bool) -> String {
    set_cookie(name, "", Duration::from_secs(0), secure)
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: &str = COOKIE_NAME_SECURE;

    #[test]
    fn a_single_pair_is_read() {
        assert_eq!(read_cookie(Some("__Host-cdash_sid=abc"), N).as_deref(), Some("abc"));
        assert_eq!(read_cookie(Some("other=1; __Host-cdash_sid=abc"), N).as_deref(), Some("abc"));
    }

    #[test]
    fn a_duplicate_is_last_wins_in_both_orderings() {
        // Assertion 10: deterministic, whichever order the duplicate arrives in.
        assert_eq!(
            read_cookie(Some("__Host-cdash_sid=first; __Host-cdash_sid=second"), N).as_deref(),
            Some("second")
        );
        assert_eq!(
            read_cookie(Some("__Host-cdash_sid=second; other=x; __Host-cdash_sid=third"), N)
                .as_deref(),
            Some("third")
        );
    }

    #[test]
    fn malformed_input_yields_none_and_never_throws() {
        for h in [
            "",
            "novalue",
            "=novalue",
            "__Host-cdash_sid",
            "__Host-cdash_sid=",
            ";;;",
            "; __Host-cdash_sid=ok;",
            "a=1;;b=2;",
        ] {
            let _ = read_cookie(Some(h), N); // must not panic
        }
        assert_eq!(read_cookie(Some("__Host-cdash_sid="), N), None, "an empty value is not a sid");
        assert_eq!(read_cookie(Some("novalue"), N), None);
        assert_eq!(read_cookie(None, N), None);
    }

    #[test]
    fn a_trailing_semicolon_and_padding_do_not_break_the_read() {
        assert_eq!(read_cookie(Some("__Host-cdash_sid=abc;"), N).as_deref(), Some("abc"));
        assert_eq!(read_cookie(Some("  __Host-cdash_sid = abc  ; x=1"), N).as_deref(), Some("abc"));
    }

    #[test]
    fn a_prefix_named_cookie_is_not_mistaken_for_ours() {
        assert_eq!(read_cookie(Some("__Host-cdash_sid_other=abc"), N), None);
        assert_eq!(read_cookie(Some("x__Host-cdash_sid=abc"), N), None);
    }

    #[test]
    fn the_secure_cookie_carries_every_attribute_the_prefix_requires() {
        let c = set_cookie(COOKIE_NAME_SECURE, "abc", Duration::from_secs(3600), true);
        assert!(c.starts_with("__Host-cdash_sid=abc"));
        for attr in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/", "Max-Age=3600"] {
            assert!(c.contains(attr), "{attr} missing from {c}");
        }
        assert!(!c.contains("Domain"), "__Host- is refused with a Domain attribute");
    }

    #[test]
    fn the_insecure_cookie_drops_the_prefix_and_secure_together() {
        // Either alone yields a cookie no browser will store, so they move
        // together or not at all.
        let c = set_cookie(COOKIE_NAME_INSECURE, "abc", Duration::from_secs(3600), false);
        assert!(c.starts_with("cdash_sid=abc"));
        assert!(!c.contains("__Host-"));
        assert!(!c.contains("Secure"));
        assert!(c.contains("HttpOnly"), "HttpOnly is not the one being dropped");
    }

    #[test]
    fn clearing_expires_the_cookie_immediately() {
        let c = clear_cookie(COOKIE_NAME_SECURE, true);
        assert!(c.contains("Max-Age=0"));
    }
}

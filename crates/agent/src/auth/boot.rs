use super::password::{parse_hash, ScryptHash};
use std::net::IpAddr;

#[derive(Debug, Clone)]
pub struct PasswordPolicy {
    pub hash: ScryptHash,
    /// When false the cookie drops both `Secure` and the `__Host-` prefix. The
    /// two move together: the prefix is refused without `Secure`, so keeping
    /// one without the other yields a cookie no browser will store.
    pub secure_cookie: bool,
}

/// Boot policy for `CDASH_AUTH=password`. Pure, so every branch is testable
/// without booting a server.
///
/// `__Host-` mandates `Secure`, so a browser reaching a plain-HTTP origin
/// discards the session cookie with no error: login returns 200, the next
/// request has no cookie, and the user loops forever seeing nothing but a
/// login page. Nothing server-side can detect that, and it reproduces only on
/// a first public deployment — so it is refused at boot instead.
pub fn decide(
    hash_env: Option<&str>,
    bind: IpAddr,
    public_url: Option<&str>,
    allow_insecure: bool,
) -> Result<PasswordPolicy, String> {
    let raw = hash_env.filter(|s| !s.is_empty()).ok_or_else(|| {
        "CDASH_AUTH includes 'password' but CDASH_PASSWORD_HASH is unset; \
         run `cdash-agent set-password`"
            .to_string()
    })?;
    let hash = parse_hash(raw)?;

    // Browsers treat http://localhost as a secure context and store the cookie
    // normally, so the failure this rule prevents cannot occur on loopback.
    // Without the exemption the rule inverts: the only way forward would strip
    // two protections the browser was willing to honour.
    if bind.is_loopback() {
        return Ok(PasswordPolicy { hash, secure_cookie: true });
    }

    if public_url.is_some_and(|u| u.starts_with("https://")) {
        return Ok(PasswordPolicy { hash, secure_cookie: true });
    }
    if allow_insecure {
        return Ok(PasswordPolicy { hash, secure_cookie: false });
    }
    Err(
        "CDASH_AUTH includes 'password' on a non-loopback bind without TLS: set \
         CDASH_PUBLIC_URL to an https:// URL, or CDASH_ALLOW_INSECURE_COOKIE=1 to accept \
         session theft on a plain-HTTP origin"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_hash() -> String {
        crate::auth::password::hash_password("a good long password").unwrap()
    }

    fn loopback() -> IpAddr {
        "127.0.0.1".parse().unwrap()
    }
    fn public() -> IpAddr {
        "0.0.0.0".parse().unwrap()
    }

    #[test]
    fn an_unset_hash_refuses_to_boot() {
        // Never a silent fall back to `none`.
        let e = decide(None, loopback(), None, false).unwrap_err();
        assert!(e.contains("CDASH_PASSWORD_HASH"), "the message must name the variable: {e}");
    }

    #[test]
    fn an_unparseable_hash_refuses_to_boot() {
        assert!(decide(Some("garbage"), loopback(), None, false).is_err());
    }

    #[test]
    fn a_public_bind_without_tls_refuses_to_boot() {
        let e = decide(Some(&a_hash()), public(), None, false).unwrap_err();
        assert!(e.contains("CDASH_PUBLIC_URL") || e.contains("CDASH_ALLOW_INSECURE_COOKIE"));
    }

    #[test]
    fn a_public_bind_with_an_https_url_boots_securely() {
        let p = decide(Some(&a_hash()), public(), Some("https://cdash.example"), false).unwrap();
        assert!(p.secure_cookie);
    }

    #[test]
    fn an_http_public_url_is_not_accepted_as_tls() {
        assert!(decide(Some(&a_hash()), public(), Some("http://cdash.example"), false).is_err());
    }

    #[test]
    fn the_insecure_escape_hatch_boots_and_drops_secure() {
        let p = decide(Some(&a_hash()), public(), None, true).unwrap();
        assert!(!p.secure_cookie, "Secure and the __Host- prefix move together");
    }

    #[test]
    fn loopback_boots_with_secure_intact_and_no_url_required() {
        // Assertion 12, the Termux posture: the safe configuration must be
        // reachable without setting the flag that would degrade it.
        for ip in ["127.0.0.1", "::1"] {
            let p = decide(Some(&a_hash()), ip.parse().unwrap(), None, false).unwrap();
            assert!(p.secure_cookie, "{ip} must keep Secure and __Host-");
        }
    }
}

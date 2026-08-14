use std::net::IpAddr;
use subtle::ConstantTimeEq;

/// Constant-time compare against `CDASH_TOKEN`. `subtle` is length-safe by
/// construction rather than by a wrapper, so unequal lengths are handled
/// without an early return that would leak length by timing.
pub fn check_bearer(header: Option<&str>, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let Some(presented) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    presented.as_bytes().ct_eq(token.as_bytes()).into()
}

/// Accept an identity header, but only from a configured upstream. Off by
/// default and unsafe unless the origin is unreachable except through the
/// proxy — anyone who can reach the origin directly can set the header.
pub fn check_trusted_proxy(peer: Option<IpAddr>, identity: Option<&str>, allow: &[IpAddr]) -> bool {
    let Some(peer) = peer else { return false };
    if !allow.contains(&peer) {
        return false;
    }
    identity.is_some_and(|i| !i.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_accepts_only_the_exact_token_with_the_scheme() {
        assert!(check_bearer(Some("Bearer s3cret"), "s3cret"));
        assert!(!check_bearer(Some("Bearer wrong"), "s3cret"));
        assert!(!check_bearer(Some("s3cret"), "s3cret"), "the scheme is required");
        assert!(!check_bearer(None, "s3cret"));
        assert!(!check_bearer(Some("Bearer "), "s3cret"));
    }

    #[test]
    fn bearer_is_length_safe() {
        // `subtle` is length-safe by construction; a prefix must not pass.
        assert!(!check_bearer(Some("Bearer s3"), "s3cret"));
        assert!(!check_bearer(Some("Bearer s3cretlonger"), "s3cret"));
    }

    #[test]
    fn bearer_never_accepts_an_empty_configured_token() {
        // Belt and braces: `AuthConfig::build` already refuses this at boot.
        assert!(!check_bearer(Some("Bearer "), ""));
        assert!(!check_bearer(Some("Bearer x"), ""));
    }

    #[test]
    fn trusted_proxy_requires_both_an_allowed_peer_and_an_identity() {
        let allow: Vec<IpAddr> = vec!["10.0.0.5".parse().unwrap()];
        assert!(check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), Some("u@x"), &allow));
        // The whole vulnerability this guard must not have: an identity header
        // accepted from an unlisted peer.
        assert!(!check_trusted_proxy(Some("10.0.0.6".parse().unwrap()), Some("u@x"), &allow));
        assert!(!check_trusted_proxy(None, Some("u@x"), &allow));
        assert!(!check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), None, &allow));
        assert!(!check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), Some(""), &allow));
    }

    #[test]
    fn trusted_proxy_with_an_empty_allowlist_accepts_nobody() {
        assert!(!check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), Some("u@x"), &[]));
    }
}

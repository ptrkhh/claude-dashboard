use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct CfConfig {
    /// e.g. `https://team.cloudflareaccess.com`
    pub team_domain: String,
    /// The Application Audience tag, which must be a member of the `aud` array.
    pub aud: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JwkKey {
    pub kid: String,
    pub n: String,
    pub e: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Jwks {
    #[serde(default)]
    pub keys: Vec<JwkKey>,
}

/// What the token proved. Either is sufficient; this is what lets one guard
/// serve browser SSO and the Tauri client's service token.
#[derive(Debug, Clone, PartialEq)]
pub enum CfIdentity {
    User(String),
    ServiceToken,
}

#[derive(Deserialize)]
struct CfClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    service_token_status: Option<bool>,
    // `common_name` is deliberately NOT read as a discriminator. It also
    // carries the common name of an mTLS client certificate, so on an
    // application that also has an mTLS policy it would admit a
    // certificate-authenticated caller as though it were the Tauri client —
    // into an origin where every authenticated caller gets RCE.
    #[serde(default)]
    common_name: Option<String>,
}

/// Verify a Cloudflare Access assertion. The key set is a parameter so every
/// case is testable without a network.
pub fn verify_cf_jwt(token: &str, jwks: &Jwks, cfg: &CfConfig) -> Result<CfIdentity, String> {
    let header = decode_header(token).map_err(|e| format!("bad JWT header: {e}"))?;
    let kid = header.kid.ok_or_else(|| "JWT has no kid".to_string())?;
    let key = jwks
        .keys
        .iter()
        .find(|k| k.kid == kid)
        .ok_or_else(|| format!("no JWKS key for kid {kid}"))?;

    let decoding =
        DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|e| format!("bad JWKS key: {e}"))?;

    // RS256 only. The algorithm is pinned here rather than read from the
    // token, which is what makes `alg: none` and algorithm confusion
    // unreachable rather than merely unlikely.
    let mut v = Validation::new(Algorithm::RS256);
    // `aud` is an array; this matches on membership, not equality.
    v.set_audience(&[&cfg.aud]);
    v.set_issuer(&[&cfg.team_domain]);

    let data = decode::<CfClaims>(token, &decoding, &v).map_err(|e| format!("JWT rejected: {e}"))?;

    if data.claims.service_token_status == Some(true) {
        return Ok(CfIdentity::ServiceToken);
    }
    if let Some(email) = data.claims.email.filter(|e| !e.is_empty()) {
        return Ok(CfIdentity::User(email));
    }
    // Logged, never trusted as the discriminator.
    if data.claims.common_name.is_some() {
        return Err("JWT carries common_name but no email or service_token_status: an mTLS \
                    identity is not a Cloudflare Access identity"
            .to_string());
    }
    Err("JWT carries neither an email nor a service token status".to_string())
}

/// JWKS is cached with periodic refresh. Cloudflare rotates keys, and a cache
/// that never refreshes would lock every user out at the rotation.
pub const JWKS_TTL: Duration = Duration::from_secs(60 * 60);

pub fn certs_url(team_domain: &str) -> String {
    format!("{}/cdn-cgi/access/certs", team_domain.trim_end_matches('/'))
}

#[derive(Default)]
struct CacheState {
    jwks: Option<Jwks>,
    fetched_at: Option<Instant>,
}

pub struct JwksCache {
    state: Mutex<CacheState>,
}

impl Default for JwksCache {
    fn default() -> Self {
        Self::new()
    }
}

impl JwksCache {
    pub fn new() -> Self {
        Self { state: Mutex::new(CacheState::default()) }
    }

    pub fn get(&self) -> Option<Jwks> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).jwks.clone()
    }

    pub fn refresh_due(&self) -> bool {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match st.fetched_at {
            None => true,
            Some(t) => t.elapsed() >= JWKS_TTL,
        }
    }

    pub fn install(&self, jwks: Jwks) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.jwks = Some(jwks);
        st.fetched_at = Some(Instant::now());
    }

    /// A failed fetch keeps the last good key set and lets the next request try
    /// again — it must not evict, because an unreachable Cloudflare would then
    /// lock out every user.
    pub fn note_failure(&self) {}
}

pub struct CfState {
    pub cfg: CfConfig,
    pub jwks: JwksCache,
}

#[cfg(test)]
#[allow(clippy::disallowed_types)] // test fixture: generates an RSA keypair, never runs at runtime
mod tests {
    use super::*;
    use base64::Engine;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;

    const TEAM: &str = "https://team.cloudflareaccess.com";
    const AUD: &str = "aud-tag-for-this-app";

    /// A 2048-bit RSA keypair generated once per test process. `openssl` keeps
    /// a private key out of the repository.
    fn keypair() -> (Vec<u8>, Jwks) {
        let dir = std::env::temp_dir().join(format!("cdash-cf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let priv_path = dir.join("k.pem");
        if !priv_path.exists() {
            let out = std::process::Command::new("openssl")
                .args(["genrsa", "-out", priv_path.to_str().unwrap(), "2048"])
                .output()
                .expect("openssl must be available for this test");
            assert!(out.status.success(), "openssl genrsa failed");
        }
        let modulus = std::process::Command::new("openssl")
            .args(["rsa", "-in", priv_path.to_str().unwrap(), "-noout", "-modulus"])
            .output()
            .unwrap();
        let hex = String::from_utf8_lossy(&modulus.stdout)
            .trim()
            .trim_start_matches("Modulus=")
            .to_string();
        let n_bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let jwks = Jwks {
            keys: vec![JwkKey {
                kid: "k1".into(),
                n: b64.encode(&n_bytes),
                e: "AQAB".into(), // 65537, the exponent openssl genrsa uses
            }],
        };
        (std::fs::read(&priv_path).unwrap(), jwks)
    }

    fn sign(pem: &[u8], claims: serde_json::Value, kid: Option<&str>) -> String {
        let mut h = Header::new(Algorithm::RS256);
        h.kid = kid.map(|k| k.to_string());
        encode(&h, &claims, &EncodingKey::from_rsa_pem(pem).unwrap()).unwrap()
    }

    fn exp_in(secs: i64) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
            as i64
            + secs
    }

    fn cfg() -> CfConfig {
        CfConfig { team_domain: TEAM.into(), aud: AUD.into() }
    }

    #[test]
    fn a_valid_user_token_yields_the_email() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "email": "u@example.com"}),
            Some("k1"),
        );
        assert_eq!(
            verify_cf_jwt(&t, &jwks, &cfg()).unwrap(),
            CfIdentity::User("u@example.com".into())
        );
    }

    #[test]
    fn a_valid_service_token_is_accepted_so_one_guard_serves_the_tauri_client() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "service_token_status": true}),
            Some("k1"),
        );
        assert_eq!(verify_cf_jwt(&t, &jwks, &cfg()).unwrap(), CfIdentity::ServiceToken);
    }

    #[test]
    fn a_common_name_without_service_token_status_is_rejected() {
        // The case the documentation check added. `common_name` also carries
        // the CN of an mTLS client certificate, so trusting it would admit a
        // certificate-authenticated caller as though it were the Tauri client.
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "common_name": "some-cert-cn"}),
            Some("k1"),
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn an_aud_array_without_the_configured_tag_is_rejected() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": ["someone-elses-app", "third"], "iss": TEAM, "exp": exp_in(3600), "email": "u@x"}),
            Some("k1"),
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn an_aud_array_containing_the_tag_among_others_is_accepted() {
        // `aud` is an array, not a string: membership, not equality.
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": ["other", AUD], "iss": TEAM, "exp": exp_in(3600), "email": "u@x"}),
            Some("k1"),
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_ok());
    }

    #[test]
    fn a_wrong_issuer_is_rejected() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": "https://attacker.cloudflareaccess.com", "exp": exp_in(3600), "email": "u@x"}),
            Some("k1"),
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(-3600), "email": "u@x"}),
            Some("k1"),
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn a_tampered_signature_is_rejected() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "email": "u@x"}),
            Some("k1"),
        );
        let mut bad = t.clone();
        bad.pop();
        bad.push(if t.ends_with('A') { 'B' } else { 'A' });
        assert!(verify_cf_jwt(&bad, &jwks, &cfg()).is_err());
    }

    #[test]
    fn an_alg_none_token_is_rejected() {
        // The specific class of bug the do-not-hand-roll rule exists to
        // prevent. `Algorithm` has no `None` variant, so this is structurally
        // unreachable — the test stays because that is a property of the crate
        // rather than of this code, and crates change.
        let (_pem, jwks) = keypair();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let h = b64.encode(br#"{"alg":"none","typ":"JWT","kid":"k1"}"#);
        let p = b64.encode(
            serde_json::to_vec(
                &json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "email": "u@x"}),
            )
            .unwrap(),
        );
        assert!(verify_cf_jwt(&format!("{h}.{p}."), &jwks, &cfg()).is_err());
    }

    #[test]
    fn an_unknown_kid_is_rejected_rather_than_matched_against_any_key() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "email": "u@x"}),
            Some("not-in-the-jwks"),
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn a_token_with_neither_an_email_nor_a_service_token_status_is_rejected() {
        let (pem, jwks) = keypair();
        let t = sign(&pem, json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600)}), Some("k1"));
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn a_service_token_status_of_false_is_not_a_service_token() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "service_token_status": false}),
            Some("k1"),
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn the_certs_url_is_derived_from_the_team_domain() {
        assert_eq!(certs_url(TEAM), "https://team.cloudflareaccess.com/cdn-cgi/access/certs");
        assert_eq!(
            certs_url("https://team.cloudflareaccess.com/"),
            "https://team.cloudflareaccess.com/cdn-cgi/access/certs",
            "a trailing slash must not double up"
        );
    }

    #[test]
    fn a_cold_cache_has_no_keys_and_is_due_for_refresh() {
        let c = JwksCache::new();
        assert!(c.get().is_none());
        assert!(c.refresh_due());
    }

    #[test]
    fn an_installed_key_set_is_served_and_not_immediately_re_fetched() {
        let c = JwksCache::new();
        c.install(Jwks { keys: vec![JwkKey { kid: "k1".into(), n: "n".into(), e: "AQAB".into() }] });
        assert_eq!(c.get().unwrap().keys.len(), 1);
        assert!(!c.refresh_due(), "a fresh key set must not be re-fetched every request");
    }

    #[test]
    fn a_failed_refresh_keeps_serving_the_last_good_key_set() {
        // Cloudflare being briefly unreachable must not lock every user out.
        let c = JwksCache::new();
        c.install(Jwks { keys: vec![JwkKey { kid: "k1".into(), n: "n".into(), e: "AQAB".into() }] });
        c.note_failure();
        assert_eq!(c.get().unwrap().keys.len(), 1, "the last good key set survives a failure");
    }
}

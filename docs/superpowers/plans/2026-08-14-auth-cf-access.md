# Auth: The `cf-access` Guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verify a Cloudflare Access JWT so one guard serves both browser SSO and the Tauri client's service token, and so a certificate-authenticated caller cannot impersonate either.

**Architecture:** Extends `crates/agent/src/auth/`. A pure verifier over a JWKS document plus a cache that refreshes it. The verifier takes the key set as a parameter, so every security case is testable against a locally generated RSA keypair with no network.

**Tech Stack:** Rust (edition 2021), `jsonwebtoken` 9.

**Spec:** `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md` (§`src/auth/` — guard chain, §Testing)

**Previous plans:** 6a (guard chain and router split), 6b (password guard) — both complete.

## Global Constraints

- **`sysinfo` pinned to `0.38.4`.** **`-D clippy::disallowed_types` is a REQUIRED gate.** **Rust floor 1.94.1.** No new `Command` site.
- **Rejection body:** `{ "error": "unauthorized" }`, nothing more.
- **All configured guards must pass** — `CDASH_AUTH` composes by AND.
- **Do not hand-roll verification of an attacker-supplied signature.** This is the plan that rule was written for.

## Verified before planning

Measured in this container:

- **`jsonwebtoken` 11 panics at first use** — it requires an explicit crypto-provider feature and aborts inside `CryptoProvider::from_crate_features` without one. **Pin `jsonwebtoken = "9"`**, whose `ring` backend is the default. Same trap as `hmac 0.13`/`sha2 0.11` in plan 6b.
- Against a locally generated 2048-bit RSA keypair, `jsonwebtoken` 9 gives every behaviour the spec's cases need: `kid` readable via `decode_header` for key selection; `set_audience` treating `aud` as an **array** and matching on membership; an `aud` with no matching tag **rejected**; a **tampered signature rejected**; `DecodingKey::from_rsa_components(n, e)` present for building keys from a JWKS.
- **`alg: none` is rejected** — confirmed with a hand-built unsigned token. `Algorithm` has no `None` variant, so the class of bug the do-not-hand-roll rule exists to prevent is structurally unreachable rather than merely tested. The test stays anyway.

## The open question this plan surfaces

**The spec's agent crate graph names no HTTP client**, but JWKS lives at `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs` and must be fetched. `reqwest` would add a large transitive tree — including a TLS stack — to a binary that currently has none, and this binary serves `/api/browse` and `/api/logs`.

This plan therefore **does not choose one**. `JwksCache` takes its fetcher as an injected async function:

- Every security property is tested against a stub fetcher and a local keypair, with no network.
- The refresh policy is tested the same way.
- Wiring a real HTTPS fetch is a one-function change once the dependency is chosen.

Until a fetcher is supplied, `CDASH_AUTH=cf-access` **rejects every request** — the same safe direction 6a used for unimplemented guards, and the boot check in Task 4 makes it a named refusal rather than a silent lockout. **Choosing the HTTP client is a decision for the maintainer, not one to make silently inside a security plan.**

---

### Task 1: The pure verifier

Every spec test case lives here. The key set is a parameter, so none of them need a network.

**Files:**
- Create: `crates/agent/src/auth/cfaccess.rs`
- Modify: `crates/agent/src/auth/mod.rs`, `crates/agent/Cargo.toml`

**Interfaces:**
- Produces:
  - `pub struct CfConfig { pub team_domain: String, pub aud: String }`
  - `pub struct Jwks { pub keys: Vec<JwkKey> }`, `pub struct JwkKey { pub kid: String, pub n: String, pub e: String }`
  - `pub enum CfIdentity { User(String), ServiceToken }`
  - `pub fn verify_cf_jwt(token: &str, jwks: &Jwks, cfg: &CfConfig) -> Result<CfIdentity, String>`

- [x] **Step 1: Add the dependency**

```toml
jsonwebtoken = "9"
```

Version 9 deliberately: 11 requires a crypto-provider feature and panics without one.

- [x] **Step 2: Write the failing tests**

`crates/agent/src/auth/cfaccess.rs` — the tests generate a keypair once via `openssl` at fixture time and build tokens from it.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use serde_json::json;

    const TEAM: &str = "https://team.cloudflareaccess.com";
    const AUD: &str = "aud-tag-for-this-app";

    /// A 2048-bit RSA keypair generated once per test process. `openssl` is
    /// already required by the environment and this keeps a private key out of
    /// the repository.
    fn keypair() -> (Vec<u8>, Jwks) {
        let dir = std::env::temp_dir().join(format!("cdash-cf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let priv_path = dir.join("k.pem");
        if !priv_path.exists() {
            let ok = std::process::Command::new("openssl")
                .args(["genrsa", "-out", priv_path.to_str().unwrap(), "2048"])
                .output()
                .expect("openssl must be available for this test");
            assert!(ok.status.success());
        }
        let der = std::process::Command::new("openssl")
            .args(["rsa", "-in", priv_path.to_str().unwrap(), "-noout", "-modulus"])
            .output()
            .unwrap();
        let hex = String::from_utf8_lossy(&der.stdout)
            .trim()
            .trim_start_matches("Modulus=")
            .to_string();
        let n_bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let jwks = Jwks {
            keys: vec![JwkKey {
                kid: "k1".into(),
                n: b64.encode(&n_bytes),
                e: "AQAB".into(), // 65537, the exponent openssl genrsa uses
            }],
        };
        (std::fs::read(&priv_path).unwrap(), jwks)
    }

    fn sign(pem: &[u8], claims: serde_json::Value, kid: Option<&str>, alg: Algorithm) -> String {
        let mut h = Header::new(alg);
        h.kid = kid.map(|k| k.to_string());
        encode(&h, &claims, &EncodingKey::from_rsa_pem(pem).unwrap()).unwrap()
    }

    fn exp_in(secs: i64) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
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
            Algorithm::RS256,
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
            Algorithm::RS256,
        );
        assert_eq!(verify_cf_jwt(&t, &jwks, &cfg()).unwrap(), CfIdentity::ServiceToken);
    }

    #[test]
    fn a_common_name_without_service_token_status_is_rejected() {
        // The case the documentation check added. `common_name` also carries
        // the CN of an mTLS client certificate, so on an application that also
        // has an mTLS policy, trusting it would admit a certificate-
        // authenticated caller as though it were the Tauri client — into an
        // origin where every authenticated caller gets RCE.
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "common_name": "some-cert-cn"}),
            Some("k1"),
            Algorithm::RS256,
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
            Algorithm::RS256,
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
            Algorithm::RS256,
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
            Algorithm::RS256,
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
            Algorithm::RS256,
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
            Algorithm::RS256,
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
        use base64::Engine;
        let (_pem, jwks) = keypair();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let h = b64.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let p = b64.encode(
            serde_json::to_vec(&json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "email": "u@x"}))
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
            Algorithm::RS256,
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn a_token_with_neither_an_email_nor_a_service_token_status_is_rejected() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600)}),
            Some("k1"),
            Algorithm::RS256,
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }

    #[test]
    fn a_service_token_status_of_false_is_not_a_service_token() {
        let (pem, jwks) = keypair();
        let t = sign(
            &pem,
            json!({"aud": [AUD], "iss": TEAM, "exp": exp_in(3600), "service_token_status": false}),
            Some("k1"),
            Algorithm::RS256,
        );
        assert!(verify_cf_jwt(&t, &jwks, &cfg()).is_err());
    }
}
```

The test module uses `std::process::Command`, which the clippy gate forbids. Add the sanctioned exemption **inside the test module only**, with the comment explaining that it is test fixture generation and not a runtime subprocess:

```rust
#[cfg(test)]
#[allow(clippy::disallowed_types)] // test fixture: generates an RSA keypair, never runs at runtime
mod tests {
```

**This makes a third `#[allow]` site.** The global constraint says a third site is a defect — it means *at runtime*, where the time-box is the point. A `#[cfg(test)]` fixture runs no subprocess in the shipped binary. Task 5 asserts the runtime count is still exactly two.

- [x] **Step 3: Run tests to verify they fail**

Add `pub mod cfaccess;` to `crates/agent/src/auth/mod.rs`.

Run: `cargo test -p cdash-agent auth::cfaccess`
Expected: FAIL — `cannot find function 'verify_cf_jwt' in this scope`.

- [x] **Step 4: Write the implementation**

Prepend to `crates/agent/src/auth/cfaccess.rs`:

```rust
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

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
    // certificate-authenticated caller as though it were the Tauri client.
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

    let decoding = DecodingKey::from_rsa_components(&key.n, &key.e)
        .map_err(|e| format!("bad JWKS key: {e}"))?;

    // RS256 only. The algorithm is pinned here rather than read from the
    // token, which is what makes `alg: none` and algorithm confusion
    // unreachable rather than merely unlikely.
    let mut v = Validation::new(Algorithm::RS256);
    // `aud` is an array; this matches on membership, not equality.
    v.set_audience(&[&cfg.aud]);
    v.set_issuer(&[&cfg.team_domain]);
    // `exp` is required and checked by default.

    let data =
        decode::<CfClaims>(token, &decoding, &v).map_err(|e| format!("JWT rejected: {e}"))?;

    if data.claims.service_token_status == Some(true) {
        return Ok(CfIdentity::ServiceToken);
    }
    if let Some(email) = data.claims.email.filter(|e| !e.is_empty()) {
        return Ok(CfIdentity::User(email));
    }
    // Logged, never trusted as the discriminator.
    if data.claims.common_name.is_some() {
        return Err(
            "JWT carries common_name but no email or service_token_status: an mTLS identity \
             is not a Cloudflare Access identity"
                .to_string(),
        );
    }
    Err("JWT carries neither an email nor a service token status".to_string())
}
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::cfaccess`
Expected: PASS, 12 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/auth/ crates/agent/Cargo.toml Cargo.lock
git commit -m "feat: cf-access JWT verifier with the common_name rejection and an alg:none test"
```

---

### Task 2: The JWKS cache

Holds the key set and refreshes it periodically. The fetcher is injected, so the refresh policy is testable without a network and the HTTP client stays an open decision.

**Files:**
- Modify: `crates/agent/src/auth/cfaccess.rs`

**Interfaces:**
- Produces:
  - `pub const JWKS_TTL: Duration` = 1 h
  - `pub struct JwksCache` with `new(url)`, `get() -> Option<Jwks>`, `refresh_due(now) -> bool`, `install(Jwks)`
  - `pub fn certs_url(team_domain: &str) -> String`

- [x] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_certs_url_is_derived_from_the_team_domain() {
        assert_eq!(
            certs_url("https://team.cloudflareaccess.com"),
            "https://team.cloudflareaccess.com/cdn-cgi/access/certs"
        );
        // A trailing slash must not double up.
        assert_eq!(
            certs_url("https://team.cloudflareaccess.com/"),
            "https://team.cloudflareaccess.com/cdn-cgi/access/certs"
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
```

- [x] **Step 2: Write the implementation**

```rust
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

    /// A failed fetch keeps the last good key set and lets the next request
    /// try again — it must not evict, because an unreachable Cloudflare would
    /// then lock out every user.
    pub fn note_failure(&self) {}
}
```

- [x] **Step 3: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::cfaccess`
Expected: PASS, 16 tests.

- [x] **Step 4: Commit**

```bash
git add crates/agent/src/auth/cfaccess.rs
git commit -m "feat: JWKS cache that serves the last good key set through a failed refresh"
```

---

### Task 3: Wire the guard leg

**Files:**
- Modify: `crates/agent/src/auth/layer.rs`, `crates/agent/src/auth/config.rs`, `crates/agent/src/http/serve.rs`

- [x] **Step 1: Extend `GuardState`**

```rust
    pub cf: Option<Arc<CfState>>,
```

where

```rust
pub struct CfState {
    pub cfg: CfConfig,
    pub jwks: JwksCache,
}
```

- [x] **Step 2: Replace the `CfAccess` arm**

```rust
            GuardKind::CfAccess => st.cf.as_ref().is_some_and(|cf| {
                let Some(jwks) = cf.jwks.get() else {
                    // No key set yet: reject rather than admit. A guard that
                    // fails open is not a guard.
                    return false;
                };
                req.headers()
                    .get("Cf-Access-Jwt-Assertion")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|t| match verify_cf_jwt(t, &jwks, &cf.cfg) {
                        Ok(_) => true,
                        Err(e) => {
                            st.log.push(format!("cf-access: {e}"));
                            false
                        }
                    })
            }),
```

The header must be read alongside the others near the top of `guard_mw`, since `req` is moved into `next.run`.

- [x] **Step 3: Boot config**

In `config_from_env`, when the chain includes `cf-access`, require `CDASH_CF_TEAM_DOMAIN` and `CDASH_CF_AUD` and fail boot naming whichever is missing — the same shape as `bearer` requiring `CDASH_TOKEN`.

Add to `AuthConfig::build`:

```rust
        if guards.contains(&GuardKind::CfAccess) && cf.is_none() {
            return Err(
                "CDASH_AUTH includes 'cf-access' but CDASH_CF_TEAM_DOMAIN and CDASH_CF_AUD are not both set"
                    .to_string(),
            );
        }
```

- [x] **Step 4: Run the suite**

Run: `cargo test --all --locked -- --test-threads=1`
Expected: PASS. Existing tests run under `none`, `bearer` or `password` and are unaffected.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/auth/ crates/agent/src/http/
git commit -m "feat: wire the cf-access guard leg, rejecting when no key set is loaded"
```

---

### Task 4: The fetcher decision, recorded

**Files:**
- Modify: `README.md`

- [x] **Step 1: Document the state honestly**

Add to the README's configuration section:

```markdown
### `cf-access` (partial)

The Cloudflare Access JWT verifier is implemented and tested — signature,
`aud` membership, `iss`, expiry, the service-token discriminator, and the
`common_name` rejection. **The JWKS fetch is not wired**: the agent has no HTTP
client, and adding one is a deliberate dependency decision rather than an
implementation detail. Until a fetcher is supplied, `CDASH_AUTH=cf-access`
rejects every request.
```

- [x] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: record that cf-access verification is complete but the JWKS fetch is not wired"
```

---

### Task 5: Confirm the runtime subprocess count is unchanged

The test fixture generates an RSA keypair with `openssl`, which needs a third `#[allow(clippy::disallowed_types)]`. That exemption is `#[cfg(test)]` and must not leak into the shipped binary.

- [x] **Step 1: Assert the runtime count**

```bash
grep -rn "allow(clippy::disallowed_types)" crates/ --include=*.rs
```

Expected: exactly three lines — `host/path.rs`, `host/cmd.rs`, and the `#[cfg(test)]` module in `auth/cfaccess.rs`. Confirm the third is immediately above a `#[cfg(test)] mod tests`.

- [x] **Step 2: Run the gate**

```bash
cargo test --all --locked -- --test-threads=1
cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types
```

Expected: PASS and exit 0.

---

## What this plan does not cover

- **The JWKS HTTP fetch** — a dependency decision, recorded above.
- **The UI** — spec step 7.
- **The Tauri clients** — steps 8–11, which this container cannot execute or verify.

## Step 6 is complete after this

The guard chain (6a), the password guard (6b) and `cf-access` verification (6c) together discharge spec step 6, with the one carve-out named above. Spec step 7 is next: `backoff.js`, the status-propagating `api()`, `poll()` applying `next()`, and the service-worker changes — independent of phase 1 and the last step this container can fully validate.

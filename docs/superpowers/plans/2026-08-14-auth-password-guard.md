# Auth: The `password` Guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A single-secret, first-party, cookie-session login served by the origin itself, so a plain browser can reach a public deployment without a third party.

**Architecture:** Extends `crates/agent/src/auth/`. An scrypt hash in an environment variable, an opaque session id in an in-memory map, a `__Host-`-prefixed cookie, and a throttle that delays rather than denies. `GET /login` and `POST /api/login` join `/api/health` as the third and second unauthenticated exceptions, bringing the enumerated list to the spec's three.

**Tech Stack:** Rust (edition 2021), `scrypt`, `subtle`, `hmac`, `sha2`, `getrandom`, `base64`, `axum` 0.8.9.

**Spec:** `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md` (§Browser authentication — the `password` guard, §Login throttling, §What `/login` may contain, §Testing 7–12)

**Previous plan:** `2026-08-14-auth-guard-chain-and-router-split.md` (6a) — complete.

## Global Constraints

- **`sysinfo` is pinned to `0.38.4`.** **`-D clippy::disallowed_types` is a REQUIRED gate.** **Rust floor 1.94.1.** No new `Command` site.
- **Rejection body.** `{ "error": "unauthorized" }` and nothing else, on every unauthenticated response.
- **All configured guards must pass** — `CDASH_AUTH` composes by AND.
- **Do not hand-roll verification of an attacker-supplied signature.** There is no signature here (that is `cf-access`, plan 6c); the KDF and the MAC both come from crates.
- **Hashing runs off the reactor**, never inline on an async worker thread.
- **scrypt parameters are `N=16384, r=8, p=1, 32-byte key`.** The *cost* is machine-dependent and is deliberately not a design constant.

## Verified before planning

Measured in this container on the pinned toolchain:

- `scrypt::Params::new(log_n, r, p)` takes **three** arguments; the key length comes from the output buffer. `N=16384` is `log_n=14`.
- `hmac 0.13` / `sha2 0.11` are the pre-1.0 line with different traits. **Pin `hmac = "0.12"`, `sha2 = "0.10"`**, whose `Hmac::<Sha256>::new_from_slice` is the documented form.
- `base64` URL-safe-no-pad of 32 random bytes is **exactly 43 characters** — the spec's sid length, which fixes the sid at 32 bytes of entropy.
- **A debug build runs scrypt in 1.08 s**, which would make the throttle tests unusable. Per-package optimisation fixes it without touching the parameters: with `[profile.dev.package.scrypt] opt-level = 3` (and the same for `salsa20`, `sha2`, `pbkdf2`) the same call takes **42 ms**, matching the spec's measured figure. This is a build-profile change, **not** a weakening of `N`.

## What this plan does not implement

**Rule A is a client-side rule.** "One login attempt per credential generation" lives in the Tauri client's `api_request`, which holds `login_attempted` per profile. There is no server-side component, and nothing here can enforce it. It belongs to the client plans (steps 8–11) and is recorded here so its absence is deliberate rather than forgotten.

---

### Task 1: The scrypt hash — parse, verify, and format

Every other piece depends on knowing whether a password was correct.

**Files:**
- Create: `crates/agent/src/auth/password.rs`
- Modify: `crates/agent/src/auth/mod.rs`
- Modify: `crates/agent/Cargo.toml`

**Interfaces:**
- Produces:
  - `pub struct ScryptHash { pub log_n: u8, pub r: u32, pub p: u32, pub salt: Vec<u8>, pub dk: Vec<u8> }`
  - `pub fn parse_hash(s: &str) -> Result<ScryptHash, String>`
  - `pub fn format_hash(h: &ScryptHash) -> String`
  - `pub fn hash_password(password: &str) -> Result<String, String>` — mints a fresh salt
  - `pub fn verify_password(password: &str, h: &ScryptHash) -> bool` — constant-time
  - `pub const MIN_PASSWORD_LEN: usize` = 12

- [ ] **Step 1: Add the dependencies and the KDF build profile**

In `crates/agent/Cargo.toml` under `[dependencies]`:

```toml
scrypt = { version = "0.12", default-features = false }
hmac = "0.12"
sha2 = "0.10"
getrandom = "0.4"
base64 = "0.22"
```

And at the **workspace root** `Cargo.toml` (profiles are only honoured there):

```toml
# The KDF is unusably slow in an unoptimised build — 1.08s per verify, measured
# — which would make the throttle tests crawl. This optimises the KDF only; it
# does not change N, r or p.
[profile.dev.package.scrypt]
opt-level = 3
[profile.dev.package.salsa20]
opt-level = 3
[profile.dev.package.sha2]
opt-level = 3
[profile.dev.package.pbkdf2]
opt-level = 3
```

- [ ] **Step 2: Write the failing tests**

`crates/agent/src/auth/password.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_round_trips_through_its_string_form() {
        let s = hash_password("correct horse battery").unwrap();
        assert!(s.starts_with("scrypt$16384$8$1$"), "the format names its parameters: {s}");
        let h = parse_hash(&s).unwrap();
        assert_eq!(h.log_n, 14);
        assert_eq!((h.r, h.p), (8, 1));
        assert_eq!(h.dk.len(), 32);
        assert_eq!(format_hash(&h), s);
    }

    #[test]
    fn verify_accepts_the_password_and_rejects_everything_else() {
        let h = parse_hash(&hash_password("correct horse battery").unwrap()).unwrap();
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("correct horse batter", &h));
        assert!(!verify_password("", &h));
        assert!(!verify_password("CORRECT HORSE BATTERY", &h));
    }

    #[test]
    fn two_hashes_of_one_password_differ_because_the_salt_is_fresh() {
        let a = hash_password("same password here").unwrap();
        let b = hash_password("same password here").unwrap();
        assert_ne!(a, b, "a reused salt would let one rainbow table serve every install");
        assert!(verify_password("same password here", &parse_hash(&a).unwrap()));
        assert!(verify_password("same password here", &parse_hash(&b).unwrap()));
    }

    #[test]
    fn an_unparseable_hash_is_an_error_not_a_panic_and_not_a_pass() {
        // Boot refuses on this; the point here is that it never yields a hash
        // that accepts something.
        for bad in [
            "",
            "not-a-hash",
            "scrypt$16384$8",
            "scrypt$16384$8$1$notbase64!!$abc",
            "bcrypt$16384$8$1$YWJj$YWJj",
            "scrypt$0$8$1$YWJj$YWJj",
        ] {
            assert!(parse_hash(bad).is_err(), "{bad} must not parse");
        }
    }

    #[test]
    fn the_minimum_length_is_enforced_at_hashing_time() {
        assert!(hash_password("short").is_err());
        assert!(hash_password(&"x".repeat(MIN_PASSWORD_LEN - 1)).is_err());
        assert!(hash_password(&"x".repeat(MIN_PASSWORD_LEN)).is_ok());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Add to `crates/agent/src/auth/mod.rs`:

```rust
pub mod password;
```

Run: `cargo test -p cdash-agent auth::password`
Expected: FAIL — `cannot find function 'hash_password' in this scope`.

- [ ] **Step 4: Write the implementation**

Prepend to `crates/agent/src/auth/password.rs`:

```rust
use base64::Engine;
use subtle::ConstantTimeEq;

/// `cdash-agent set-password` refuses anything shorter. One secret guards an
/// origin where every authenticated caller gets RCE.
pub const MIN_PASSWORD_LEN: usize = 12;

const LOG_N: u8 = 14; // N = 16384
const R: u32 = 8;
const P: u32 = 1;
const DK_LEN: usize = 32;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScryptHash {
    pub log_n: u8,
    pub r: u32,
    pub p: u32,
    pub salt: Vec<u8>,
    pub dk: Vec<u8>,
}

/// `scrypt$<N>$<r>$<p>$<salt>$<dk>`. N is written expanded rather than as
/// log2 so the stored value is readable against the spec.
pub fn format_hash(h: &ScryptHash) -> String {
    format!(
        "scrypt${}${}${}${}${}",
        1u32 << h.log_n,
        h.r,
        h.p,
        b64().encode(&h.salt),
        b64().encode(&h.dk)
    )
}

pub fn parse_hash(s: &str) -> Result<ScryptHash, String> {
    let parts: Vec<&str> = s.split('$').collect();
    if parts.len() != 6 || parts[0] != "scrypt" {
        return Err("CDASH_PASSWORD_HASH is not a scrypt$N$r$p$salt$dk string".to_string());
    }
    let n: u32 = parts[1].parse().map_err(|_| "bad N".to_string())?;
    if n < 2 || !n.is_power_of_two() {
        return Err(format!("bad N: {n}"));
    }
    let log_n = n.trailing_zeros() as u8;
    let r: u32 = parts[2].parse().map_err(|_| "bad r".to_string())?;
    let p: u32 = parts[3].parse().map_err(|_| "bad p".to_string())?;
    let salt = b64().decode(parts[4]).map_err(|_| "bad salt".to_string())?;
    let dk = b64().decode(parts[5]).map_err(|_| "bad dk".to_string())?;
    if salt.is_empty() || dk.is_empty() {
        return Err("empty salt or key".to_string());
    }
    Ok(ScryptHash { log_n, r, p, salt, dk })
}

fn derive(password: &str, salt: &[u8], log_n: u8, r: u32, p: u32, out: &mut [u8]) -> Result<(), String> {
    let params = scrypt::Params::new(log_n, r, p).map_err(|e| e.to_string())?;
    scrypt::scrypt(password.as_bytes(), salt, &params, out).map_err(|e| e.to_string())
}

pub fn hash_password(password: &str) -> Result<String, String> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!("password must be at least {MIN_PASSWORD_LEN} characters"));
    }
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
    let mut dk = [0u8; DK_LEN];
    derive(password, &salt, LOG_N, R, P, &mut dk)?;
    Ok(format_hash(&ScryptHash {
        log_n: LOG_N,
        r: R,
        p: P,
        salt: salt.to_vec(),
        dk: dk.to_vec(),
    }))
}

/// Constant-time via `subtle`, which is length-safe by construction. Run this
/// on a blocking thread — it is a deliberately expensive function.
pub fn verify_password(password: &str, h: &ScryptHash) -> bool {
    let mut dk = vec![0u8; h.dk.len()];
    if derive(password, &h.salt, h.log_n, h.r, h.p, &mut dk).is_err() {
        return false;
    }
    dk.ct_eq(&h.dk).into()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::password`
Expected: PASS, 5 tests, in a couple of seconds. **If a single test takes more than ~2 s, the build-profile block did not land** — check it is at the workspace root, not in `crates/agent/`.

- [ ] **Step 6: Commit**

```bash
git add crates/agent/src/auth/ crates/agent/Cargo.toml Cargo.toml Cargo.lock
git commit -m "feat: scrypt password hashing with a constant-time verify"
```

---

### Task 2: The session store

An opaque id in a map. No algorithm to influence, expiry is a number the server owns, `delete` is a working logout, and a restart is a working panic button.

**Files:**
- Create: `crates/agent/src/auth/session.rs`
- Modify: `crates/agent/src/auth/mod.rs`

**Interfaces:**
- Produces:
  - `pub const SESSION_TTL: Duration` = 12 h, `pub const SID_BYTES: usize` = 32
  - `pub struct Sessions` with `new`, `mint() -> String`, `is_valid(&str) -> bool`, `revoke(&str)`, `len()`

- [ ] **Step 1: Write the failing tests**

`crates/agent/src/auth/session.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_sid_is_43_url_safe_characters() {
        let s = Sessions::new();
        let sid = s.mint();
        assert_eq!(sid.len(), 43, "32 bytes of entropy, base64url unpadded");
        assert!(sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn two_sids_are_never_equal() {
        let s = Sessions::new();
        let a: std::collections::HashSet<String> = (0..256).map(|_| s.mint()).collect();
        assert_eq!(a.len(), 256, "the CSPRNG must not repeat");
    }

    #[test]
    fn a_minted_sid_validates_and_an_unknown_one_does_not() {
        let s = Sessions::new();
        let sid = s.mint();
        assert!(s.is_valid(&sid));
        assert!(!s.is_valid("not-a-session"));
        assert!(!s.is_valid(""));
    }

    #[test]
    fn revoke_is_a_working_logout() {
        let s = Sessions::new();
        let sid = s.mint();
        s.revoke(&sid);
        assert!(!s.is_valid(&sid));
    }

    #[test]
    fn an_expired_entry_is_rejected_at_lookup_and_needs_no_sweeper() {
        // The store has no background task: entries are minted only by a
        // successful login, and expiry is checked on the way in.
        let s = Sessions::new();
        let sid = s.mint_with_expiry(std::time::Instant::now() - Duration::from_secs(1));
        assert!(!s.is_valid(&sid), "an expired session must not authenticate");
    }

    #[test]
    fn the_lifetime_is_absolute_with_no_sliding_renewal() {
        // A stolen cookie is RCE; 12 hours is the bound, and using a session
        // must not extend it.
        assert_eq!(SESSION_TTL, Duration::from_secs(12 * 60 * 60));
        let s = Sessions::new();
        let sid = s.mint();
        let first = s.expires_at(&sid).unwrap();
        assert!(s.is_valid(&sid));
        assert_eq!(s.expires_at(&sid).unwrap(), first, "using it must not renew it");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod session;` to `crates/agent/src/auth/mod.rs`.

Run: `cargo test -p cdash-agent auth::session`
Expected: FAIL — `cannot find type 'Sessions' in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/agent/src/auth/session.rs`:

```rust
use base64::Engine;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 12-hour absolute lifetime, no sliding renewal. Conventional 7–30 day
/// sessions are calibrated for accounts that can be re-secured after theft;
/// this one cannot be — a stolen cookie is RCE as the running user.
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
pub const SID_BYTES: usize = 32;

/// An opaque id in a map. A stateless token would need a signature, a pinned
/// algorithm, and a re-checked expiry, and still could not be revoked.
pub struct Sessions {
    map: Mutex<HashMap<String, Instant>>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self::new()
    }
}

impl Sessions {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    pub fn mint(&self) -> String {
        self.mint_with_expiry(Instant::now() + SESSION_TTL)
    }

    pub fn mint_with_expiry(&self, expires_at: Instant) -> String {
        let mut raw = [0u8; SID_BYTES];
        getrandom::fill(&mut raw).expect("the OS CSPRNG must be available");
        let sid = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        self.map.lock().unwrap_or_else(|e| e.into_inner()).insert(sid.clone(), expires_at);
        sid
    }

    pub fn is_valid(&self, sid: &str) -> bool {
        if sid.is_empty() {
            return false;
        }
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.get(sid).is_some_and(|exp| *exp > Instant::now())
    }

    pub fn expires_at(&self, sid: &str) -> Option<Instant> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(sid).copied()
    }

    pub fn revoke(&self, sid: &str) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::session`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/auth/
git commit -m "feat: opaque session store with a 12-hour absolute lifetime"
```

---

### Task 3: The cookie splitter and the `Set-Cookie` builder

The one piece of hand-rolled parsing on attacker-influenced input. It is exempt from "do not hand-roll" not because the rule fails to apply but because it is small enough for a test to discharge it. Spec assertion 10.

**Files:**
- Create: `crates/agent/src/auth/cookie.rs`
- Modify: `crates/agent/src/auth/mod.rs`

**Interfaces:**
- Produces:
  - `pub const COOKIE_NAME_SECURE: &str` = `"__Host-cdash_sid"`, `pub const COOKIE_NAME_INSECURE: &str` = `"cdash_sid"`
  - `pub fn read_cookie(header: Option<&str>, name: &str) -> Option<String>`
  - `pub fn set_cookie(name: &str, sid: &str, ttl: Duration, secure: bool) -> String`
  - `pub fn clear_cookie(name: &str, secure: bool) -> String`

- [ ] **Step 1: Write the failing tests**

`crates/agent/src/auth/cookie.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod cookie;` to `crates/agent/src/auth/mod.rs`.

Run: `cargo test -p cdash-agent auth::cookie`
Expected: FAIL — `cannot find value 'COOKIE_NAME_SECURE' in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/agent/src/auth/cookie.rs`:

```rust
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
/// piece of hand-rolled parsing on attacker-influenced input.
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
    let mut c = format!(
        "{name}={sid}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        ttl.as_secs()
    );
    if secure {
        c.push_str("; Secure");
    }
    c
}

pub fn clear_cookie(name: &str, secure: bool) -> String {
    set_cookie(name, "", Duration::from_secs(0), secure)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::cookie`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/auth/
git commit -m "feat: cookie splitter with last-wins semantics, and the __Host- builder"
```

---

### Task 4: The throttle — delay, never deny

Rules B and C. Rule A is client-side and out of scope, as recorded above.

**Files:**
- Create: `crates/agent/src/auth/throttle.rs`
- Modify: `crates/agent/src/auth/mod.rs`

**Interfaces:**
- Produces:
  - `pub const FREE_ATTEMPTS: u32` = 5, `pub const MAX_DELAY: Duration` = 20 s, `pub const IDLE_RESET: Duration` = 15 min, `pub const DEFAULT_PENDING_MAX: usize` = 1024
  - `pub fn delay_for(distinct_failures: u32) -> Duration` — pure
  - `pub struct Throttle` with `new(pending_max)`, `admit() -> Option<Guard>`, `note_failure(&str)`, `note_success()`, `distinct_failures()`

- [ ] **Step 1: Write the failing tests**

`crates/agent/src/auth/throttle.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_five_attempts_are_not_delayed() {
        for n in 0..=FREE_ATTEMPTS {
            assert_eq!(delay_for(n), Duration::ZERO, "attempt {n} must not be delayed");
        }
    }

    #[test]
    fn the_ladder_doubles_and_is_capped_at_twenty_seconds() {
        assert_eq!(delay_for(6), Duration::from_secs(2));
        assert_eq!(delay_for(7), Duration::from_secs(4));
        assert_eq!(delay_for(8), Duration::from_secs(8));
        assert_eq!(delay_for(9), Duration::from_secs(16));
        assert_eq!(delay_for(10), MAX_DELAY, "capped, not unbounded");
        assert_eq!(delay_for(50), MAX_DELAY);
        assert_eq!(delay_for(u32::MAX), MAX_DELAY, "no overflow panic on a long attack");
    }

    #[test]
    fn replaying_one_password_does_not_advance_the_counter() {
        // Rule B: a stale client repeats itself; a brute-forcer must vary to
        // learn anything.
        let t = Throttle::new(DEFAULT_PENDING_MAX);
        for _ in 0..50 {
            t.note_failure("same wrong password");
        }
        assert_eq!(t.distinct_failures(), 1, "50 replays of one credential is one distinct failure");
    }

    #[test]
    fn distinct_passwords_each_advance_the_counter() {
        let t = Throttle::new(DEFAULT_PENDING_MAX);
        for i in 0..40 {
            t.note_failure(&format!("guess-{i}"));
        }
        assert_eq!(t.distinct_failures(), 40);
    }

    #[test]
    fn alternating_two_wrong_passwords_is_worse_for_the_attacker_than_distinct_guessing() {
        // Only one fingerprint is retained, so A,B,A,B advances every time —
        // strictly worse than 2 distinct guesses, and never better.
        let t = Throttle::new(DEFAULT_PENDING_MAX);
        for _ in 0..5 {
            t.note_failure("a");
            t.note_failure("b");
        }
        assert!(t.distinct_failures() >= 2);
    }

    #[test]
    fn success_resets_the_counter() {
        let t = Throttle::new(DEFAULT_PENDING_MAX);
        for i in 0..10 {
            t.note_failure(&format!("g{i}"));
        }
        t.note_success();
        assert_eq!(t.distinct_failures(), 0);
    }

    #[test]
    fn admission_is_bounded_and_overflow_is_the_only_refusal() {
        // A login attempt is never rejected for throttle reasons; the only
        // refusal is volumetric overflow, and it releases as guards drop.
        let t = Throttle::new(2);
        let a = t.admit().expect("first admits");
        let b = t.admit().expect("second admits");
        assert!(t.admit().is_none(), "third exceeds the pending bound");
        drop(a);
        assert!(t.admit().is_some(), "a completed attempt frees a slot");
        drop(b);
    }

    #[test]
    fn the_fingerprint_never_retains_the_password() {
        // The retained value is an HMAC under a boot-random key: no growth,
        // no stored password, no reusable hash.
        let t = Throttle::new(DEFAULT_PENDING_MAX);
        t.note_failure("hunter2 is the password");
        assert!(
            !format!("{:?}", t.last_fingerprint()).contains("hunter2"),
            "the plaintext must never be retained"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod throttle;` to `crates/agent/src/auth/mod.rs`.

Run: `cargo test -p cdash-agent auth::throttle`
Expected: FAIL — `cannot find function 'delay_for' in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/agent/src/auth/throttle.rs`:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FREE_ATTEMPTS: u32 = 5;
pub const MAX_DELAY: Duration = Duration::from_secs(20);
pub const IDLE_RESET: Duration = Duration::from_secs(15 * 60);
/// Derived from what a pending request actually costs — one socket and one
/// entry in a wake list. At 4 the sustained rate would be 0.2 req/s, which is
/// trivially cheap denial; at 1024 it is 51.2 req/s, i.e. ordinary volumetric
/// load that belongs to the reverse proxy.
pub const DEFAULT_PENDING_MAX: usize = 1024;

/// `min(1s · 2^(n−5), 20s)`, and zero for the first five.
pub fn delay_for(distinct_failures: u32) -> Duration {
    if distinct_failures <= FREE_ATTEMPTS {
        return Duration::ZERO;
    }
    let shift = distinct_failures - FREE_ATTEMPTS;
    // Saturating: a long attack must not overflow into a short delay.
    let secs = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_DELAY)
}

struct State {
    distinct: u32,
    last_fingerprint: Option<[u8; 32]>,
    last_seen: Instant,
}

/// Held for the lifetime of one login attempt; dropping it frees a slot.
pub struct Guard {
    pending: Arc<AtomicUsize>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Throttle {
    state: Mutex<State>,
    key: [u8; 32],
    pending: Arc<AtomicUsize>,
    pending_max: usize,
}

impl Throttle {
    pub fn new(pending_max: usize) -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("the OS CSPRNG must be available");
        Self {
            state: Mutex::new(State {
                distinct: 0,
                last_fingerprint: None,
                last_seen: Instant::now(),
            }),
            key,
            pending: Arc::new(AtomicUsize::new(0)),
            pending_max,
        }
    }

    /// `None` only on volumetric overflow, which the caller renders as 503 with
    /// `Retry-After`. Never a throttle-reasoned rejection.
    pub fn admit(&self) -> Option<Guard> {
        let n = self.pending.fetch_add(1, Ordering::SeqCst);
        if n >= self.pending_max {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Guard { pending: Arc::clone(&self.pending) })
    }

    fn fingerprint(&self, password: &str) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key size");
        mac.update(password.as_bytes());
        let out = mac.finalize().into_bytes();
        let mut fp = [0u8; 32];
        fp.copy_from_slice(&out);
        fp
    }

    /// Rule B: a failure whose fingerprint equals the previous failure's does
    /// not advance the counter. One value retained — no growth, no stored
    /// password, no reusable hash.
    pub fn note_failure(&self, password: &str) {
        let fp = self.fingerprint(password);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.last_seen.elapsed() >= IDLE_RESET {
            st.distinct = 0;
            st.last_fingerprint = None;
        }
        st.last_seen = Instant::now();
        if st.last_fingerprint == Some(fp) {
            return;
        }
        st.last_fingerprint = Some(fp);
        st.distinct = st.distinct.saturating_add(1);
    }

    pub fn note_success(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.distinct = 0;
        st.last_fingerprint = None;
        st.last_seen = Instant::now();
    }

    pub fn distinct_failures(&self) -> u32 {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.last_seen.elapsed() >= IDLE_RESET {
            st.distinct = 0;
            st.last_fingerprint = None;
        }
        st.distinct
    }

    pub fn last_fingerprint(&self) -> Option<[u8; 32]> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).last_fingerprint
    }

    pub fn current_delay(&self) -> Duration {
        delay_for(self.distinct_failures())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::throttle`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/auth/
git commit -m "feat: login throttle that delays rather than denies, with a fingerprint counter"
```

---

### Task 5: Boot policy — the refusals and the loopback exemption

A misconfiguration that cannot be diagnosed from its symptom must be refused at boot rather than debugged in production. Spec assertions 11 and 12.

**Files:**
- Create: `crates/agent/src/auth/boot.rs`
- Modify: `crates/agent/src/auth/mod.rs`

**Interfaces:**
- Produces:
  - `pub struct PasswordPolicy { pub hash: ScryptHash, pub secure_cookie: bool }`
  - `pub fn decide(hash_env: Option<&str>, bind: IpAddr, public_url: Option<&str>, allow_insecure: bool) -> Result<PasswordPolicy, String>` — pure

- [ ] **Step 1: Write the failing tests**

`crates/agent/src/auth/boot.rs`:

```rust
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
        // `__Host-` mandates Secure, so a browser on plain HTTP discards the
        // cookie with no error and the user sees an endless login loop.
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
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod boot;` to `crates/agent/src/auth/mod.rs`.

Run: `cargo test -p cdash-agent auth::boot`
Expected: FAIL — `cannot find function 'decide' in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/agent/src/auth/boot.rs`:

```rust
use super::password::{parse_hash, ScryptHash};
use std::net::IpAddr;

pub struct PasswordPolicy {
    pub hash: ScryptHash,
    /// When false the cookie drops both `Secure` and the `__Host-` prefix. The
    /// two move together: the prefix is refused without `Secure`, so keeping
    /// one without the other yields a cookie no browser will store.
    pub secure_cookie: bool,
}

/// Boot policy for `CDASH_AUTH=password`. Pure, so every branch is testable
/// without booting a server.
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::boot`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/auth/
git commit -m "feat: password boot policy with the loopback exemption"
```

---

### Task 6: The login routes and the guard leg

Wires the four pieces together: `/login`, `POST /api/login`, `POST /api/logout`, and the `password` leg of the guard.

**Files:**
- Create: `crates/agent/src/auth/login.rs`
- Modify: `crates/agent/src/auth/mod.rs`, `crates/agent/src/auth/layer.rs`, `crates/agent/src/http/serve.rs`

**Interfaces:**
- Produces:
  - `pub struct PasswordState { pub policy: Arc<PasswordPolicy>, pub sessions: Arc<Sessions>, pub throttle: Arc<Throttle> }`
  - `pub const LOGIN_HTML: &str`
  - `pub async fn get_login() -> Response`, `pub async fn post_login(...) -> Response`, `pub async fn post_logout(...) -> Response`

- [ ] **Step 1: Write the failing tests**

`crates/agent/src/auth/login.rs`:

```rust
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
        for asset in ["<script", "<img", "href=\"http", "src=\"", "favicon", "stylesheet"] {
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
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod login;` to `crates/agent/src/auth/mod.rs`.

Run: `cargo test -p cdash-agent auth::login`
Expected: FAIL — `cannot find value 'LOGIN_HTML' in this scope`.

- [ ] **Step 3: Write the page and the routes**

Prepend to `crates/agent/src/auth/login.rs`:

```rust
use super::boot::PasswordPolicy;
use super::cookie::{clear_cookie, read_cookie, set_cookie, COOKIE_NAME_INSECURE, COOKIE_NAME_SECURE};
use super::password::verify_password;
use super::session::{Sessions, SESSION_TTL};
use super::throttle::Throttle;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

/// A password field, a submit button, an error region. No product name, no
/// version, no logo, no favicon reference, and no title beyond "Sign in".
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

pub async fn post_login(
    State(st): State<PasswordState>,
    Json(body): Json<LoginBody>,
) -> Response {
    // The only refusal, and it is volumetric rather than throttle-reasoned.
    let Some(_slot) = st.throttle.admit() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response();
    };

    // Delayed before evaluation, then processed normally.
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

pub async fn post_logout(
    State(st): State<PasswordState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(sid) = read_cookie(
        headers.get(header::COOKIE).and_then(|v| v.to_str().ok()),
        st.cookie_name(),
    ) {
        st.sessions.revoke(&sid);
    }
    (
        StatusCode::OK,
        [(header::SET_COOKIE, clear_cookie(st.cookie_name(), st.policy.secure_cookie))],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}
```

- [ ] **Step 4: Wire the guard leg and the routes**

In `crates/agent/src/auth/layer.rs`, extend `UNAUTH_PATHS` to the spec's three and give `GuardState` the password state:

```rust
pub const UNAUTH_PATHS: &[&str] = &["/api/health", "/login", "/api/login"];
```

```rust
#[derive(Clone)]
pub struct GuardState {
    pub auth: Arc<AuthConfig>,
    pub log: Arc<LogBuffer>,
    pub password: Option<super::login::PasswordState>,
}
```

Replace the `Password` arm of the match:

```rust
            GuardKind::Password => st
                .password
                .as_ref()
                .is_some_and(|p| p.authenticated(cookie.as_deref())),
```

reading the cookie header alongside the others:

```rust
    let cookie = req
        .headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
```

A non-`/api` path that fails the guard redirects rather than 401ing, so the browser lands on the login page:

```rust
        if !ok {
            st.log.push(format!("auth: rejected by {g:?}"));
            let is_api = req.uri().path().starts_with("/api/");
            return if is_api {
                unauthorized()
            } else {
                (StatusCode::FOUND, [(axum::http::header::LOCATION, "/login")]).into_response()
            };
        }
```

In `crates/agent/src/http/serve.rs`, add the two unauthenticated routes and `/api/logout` to the guarded half:

```rust
    post "/api/logout" => routes::post_logout,
```

and in `router`, when a password policy exists:

```rust
    let mut unauth = Router::new()
        .route("/api/health", get(|| async { Json(serde_json::json!({ "ok": true })) }));
    if let Some(pw) = password.clone() {
        unauth = unauth.merge(
            Router::new()
                .route("/login", get(crate::auth::login::get_login))
                .route("/api/login", post(crate::auth::login::post_login))
                .with_state(pw),
        );
    }
```

`routes::post_logout` is a thin wrapper that pulls `PasswordState` out of `Ctx`; add it to `crates/agent/src/http/routes.rs`.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test -p cdash-agent -- --test-threads=1`
Expected: PASS. Every existing test runs under `CDASH_AUTH=none`, where the guard is a pass-through and `password` is `None`.

- [ ] **Step 6: Commit**

```bash
git add crates/agent/src/auth/ crates/agent/src/http/
git commit -m "feat: /login, /api/login, /api/logout, and the password guard leg"
```

---

### Task 7: `set-password`

A subcommand, reading from the terminal with echo suppressed. Never writes a file, never echoes.

**Files:**
- Modify: `crates/agent/src/main.rs`

- [ ] **Step 1: Write the implementation**

In `crates/agent/src/main.rs`, before the server starts:

```rust
    // `cdash-agent set-password` — prints the hash to stdout for the operator
    // to place in the environment. It never writes a file: this process serves
    // /api/browse and /api/logs, so a secret on its disk is one disclosure away
    // from total compromise.
    if std::env::args().nth(1).as_deref() == Some("set-password") {
        match read_password_twice() {
            Ok(hash) => {
                println!("{hash}");
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
```

and the helper:

```rust
fn read_password_twice() -> Result<String, String> {
    let a = prompt_hidden("Password: ")?;
    let b = prompt_hidden("Again: ")?;
    if a != b {
        return Err("passwords did not match".to_string());
    }
    cdash_agent::auth::password::hash_password(&a)
}

/// Echo suppression via `termios`, so the password never reaches the terminal
/// scrollback. `rustix` is already a dependency.
fn prompt_hidden(prompt: &str) -> Result<String, String> {
    use std::io::{BufRead, Write};
    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    let tty = std::io::stdin();
    let saved = rustix::termios::tcgetattr(&tty).map_err(|e| e.to_string())?;
    let mut raw = saved.clone();
    raw.local_modes -= rustix::termios::LocalModes::ECHO;
    rustix::termios::tcsetattr(&tty, rustix::termios::OptionalActions::Now, &raw)
        .map_err(|e| e.to_string())?;

    let mut line = String::new();
    let read = tty.lock().read_line(&mut line);
    // Restore the terminal whatever happened.
    let _ = rustix::termios::tcsetattr(&tty, rustix::termios::OptionalActions::Now, &saved);
    eprintln!();
    read.map_err(|e| e.to_string())?;

    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}
```

`rustix` needs the `termios` feature: in `crates/agent/Cargo.toml`, `rustix = { version = "1", features = ["fs", "termios"] }`.

- [ ] **Step 2: Verify by hand**

Run:

```bash
printf 'a good long password\na good long password\n' | cargo run -q -p cdash-agent set-password
```

Expected: one `scrypt$16384$8$1$...` line on stdout and nothing else. Then confirm it round-trips:

```bash
H=$(printf 'a good long password\na good long password\n' | cargo run -q -p cdash-agent set-password)
CDASH_AUTH=password CDASH_PASSWORD_HASH="$H" CDASH_BIND=127.0.0.1 PORT=8096 cargo run -q -p cdash-agent &
sleep 3
curl -s -o /dev/null -w '%{http_code}\n' --noproxy '*' http://127.0.0.1:8096/api/sessions          # 401
curl -s -o /dev/null -w '%{http_code}\n' --noproxy '*' http://127.0.0.1:8096/login                  # 200
curl -s -i --noproxy '*' -H 'content-type: application/json' \
  -d '{"password":"a good long password"}' http://127.0.0.1:8096/api/login | grep -i set-cookie
```

Expected: 401, 200, and a `Set-Cookie` carrying `__Host-cdash_sid`, `HttpOnly`, `Secure`, `SameSite=Lax`.

Record the observed output in the commit message.

- [ ] **Step 3: Commit**

```bash
git add crates/agent/src/main.rs crates/agent/Cargo.toml Cargo.lock
git commit -m "feat: set-password subcommand with echo suppression"
```

---

### Task 8: Integration assertions 7–12

**Files:**
- Create: `crates/agent/tests/password_integration.rs`

- [ ] **Step 1: Write the tests**

The suite mirrors `auth_integration.rs`'s raw-HTTP helper. Assertions, each named for its spec number:

- **7** — `GET /login` → 200 unauthenticated; `POST /api/login` wrong → 401 with **no** `Set-Cookie`; right → 200 with `Set-Cookie` carrying `HttpOnly`, `Secure`, `SameSite=Lax`, `__Host-`. Then replay assertions 2 and 3 **with the cookie** and require 200 for `/api/sessions` and 200 for `/`.
- **8** — CSRF: a POST to `/api/kill` **with a valid cookie** and `content-type: text/plain` → **415**, likewise `application/x-www-form-urlencoded`, `multipart/form-data`, and **no content-type at all**; assert the side effect did not occur (the session's meta entry survives); `{` under a correct content-type → **400**; `{"name":123}` → **422**; and no response carries `Access-Control-Allow-Origin`.
- **9** — Throttle: arm with 6 distinct wrong passwords, then a correct login returns **200 after a delay and never 429**; a second login issued while one is pending also returns 200; replaying one wrong password does not advance the counter (assert via elapsed time staying below the next rung).
- **10** — covered as a unit in Task 3; the integration suite asserts the end-to-end consequence: two `__Host-cdash_sid` cookies in one header authenticate as the last one.
- **11** — Boot refusals: `decide` returns `Err` for the unset hash, the unparseable hash, and the public-bind-without-TLS case; with `CDASH_ALLOW_INSECURE_COOKIE=1` the `Set-Cookie` carries **neither** `Secure` **nor** `__Host-`.
- **12** — Loopback exemption: bind `127.0.0.1` with no URL and no flag **boots**, and its `Set-Cookie` carries `Secure` **and** `__Host-`.

Plus the spec's assertion 5 with its **exact** pair: boot `CDASH_AUTH=bearer,password` and assert 401 with a valid bearer alone, 401 with a valid cookie alone, 200 with both. 6a pinned this with `bearer,trusted-proxy`; this replaces the substitute.

- [ ] **Step 2: Run and iterate**

Run: `cargo test -p cdash-agent --test password_integration -- --test-threads=1`
Expected: PASS. A failure here is a guard defect, not a test to relax.

- [ ] **Step 3: Verify the CSRF assertion is falsifiable**

Temporarily change `post_kill` to take `body: String` instead of `Json<NameBody>` and re-run: assertion 8 must fail with 200 instead of 415. Revert.

- [ ] **Step 4: Full gate and commit**

```bash
cargo test --all --locked -- --test-threads=1
cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types
git add crates/agent/tests/password_integration.rs
git commit -m "test: password guard integration, assertions 7-12"
```

---

## What this plan does not cover

- **Rule A** — client-side, steps 8–11.
- **`cf-access`** — plan 6c.
- **The UI** — spec step 7. `public/app.js` does not yet handle a 401 by redirecting to `/login`; that is the UI plan's `api()` change.

## Next plan starts here

Plan 6c: `cf-access`. RS256 verification against cached JWKS, `aud` as an array, `iss` equal to the team domain, `service_token_status` as the service-token discriminator, and `common_name` rejected — with the `alg: none` test the do-not-hand-roll rule exists to demand.

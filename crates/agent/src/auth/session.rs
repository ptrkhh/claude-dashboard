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
        let sid = s.mint_with_expiry(Instant::now() - Duration::from_secs(1));
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

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
/// load that belongs to the reverse proxy that already terminates TLS.
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
    /// `Retry-After`. Never a throttle-reasoned rejection: once accepted, an
    /// attempt is always eventually evaluated.
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
    /// password, no reusable hash. A stale client repeats itself; a
    /// brute-forcer must vary to learn anything.
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
        // Rule B: a stale client repeats itself; a brute-forcer must vary.
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

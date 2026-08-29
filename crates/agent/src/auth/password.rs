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

fn derive(
    password: &str,
    salt: &[u8],
    log_n: u8,
    r: u32,
    p: u32,
    out: &mut [u8],
) -> Result<(), String> {
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

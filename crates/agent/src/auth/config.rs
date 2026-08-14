use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    None,
    Bearer,
    CfAccess,
    TrustedProxy,
    Password,
}

/// Parse the comma-composable `CDASH_AUTH`. An unrecognised leg is an error,
/// never a silent fall back to `none`: this origin runs every session with
/// `--dangerously-skip-permissions`.
pub fn parse_auth(spec: &str) -> Result<Vec<GuardKind>, String> {
    let legs: Vec<&str> = spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if legs.is_empty() {
        return Ok(vec![GuardKind::None]);
    }
    let mut out = Vec::new();
    for leg in &legs {
        out.push(match *leg {
            "none" => GuardKind::None,
            "bearer" => GuardKind::Bearer,
            "cf-access" => GuardKind::CfAccess,
            "trusted-proxy" => GuardKind::TrustedProxy,
            "password" => GuardKind::Password,
            other => return Err(format!("unknown CDASH_AUTH value: {other}")),
        });
    }
    if out.len() > 1 && out.contains(&GuardKind::None) {
        return Err("CDASH_AUTH: 'none' cannot be composed with another guard".to_string());
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub guards: Vec<GuardKind>,
    pub token: Option<String>,
    pub proxy_header: String,
    pub proxy_allow: Vec<IpAddr>,
}

impl AuthConfig {
    /// Every guard's required inputs are checked here, at boot, rather than on
    /// the first request that needs them.
    pub fn build(
        guards: Vec<GuardKind>,
        token: Option<String>,
        proxy_header: String,
        proxy_allow: Vec<IpAddr>,
    ) -> Result<Self, String> {
        if guards.contains(&GuardKind::Bearer) && token.as_deref().unwrap_or("").is_empty() {
            return Err("CDASH_AUTH includes 'bearer' but CDASH_TOKEN is unset".to_string());
        }
        if guards.contains(&GuardKind::TrustedProxy) && proxy_allow.is_empty() {
            return Err(
                "CDASH_AUTH includes 'trusted-proxy' but CDASH_PROXY_ALLOW names no upstream IP"
                    .to_string(),
            );
        }
        Ok(Self { guards, token, proxy_header, proxy_allow })
    }

    pub fn is_open(&self) -> bool {
        self.guards == [GuardKind::None]
    }
}

pub fn config_from_env() -> Result<AuthConfig, String> {
    let guards = parse_auth(&std::env::var("CDASH_AUTH").unwrap_or_default())?;
    let proxy_allow = std::env::var("CDASH_PROXY_ALLOW")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<IpAddr>().map_err(|_| format!("CDASH_PROXY_ALLOW: bad IP: {s}")))
        .collect::<Result<Vec<_>, _>>()?;
    AuthConfig::build(
        guards,
        std::env::var("CDASH_TOKEN").ok(),
        std::env::var("CDASH_PROXY_HEADER").unwrap_or_else(|_| "X-Forwarded-Email".to_string()),
        proxy_allow,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_absent_setting_is_the_local_default() {
        assert_eq!(parse_auth("").unwrap(), vec![GuardKind::None]);
        assert_eq!(parse_auth("none").unwrap(), vec![GuardKind::None]);
    }

    #[test]
    fn every_named_leg_is_kept() {
        // The defect this guards: a parse that drops a leg is invisible to
        // every other test, because one working guard looks like a chain.
        assert_eq!(
            parse_auth("bearer,password").unwrap(),
            vec![GuardKind::Bearer, GuardKind::Password]
        );
        assert_eq!(
            parse_auth("password,cf-access").unwrap(),
            vec![GuardKind::Password, GuardKind::CfAccess]
        );
        assert_eq!(parse_auth("bearer, trusted-proxy ").unwrap().len(), 2, "whitespace is trimmed");
    }

    #[test]
    fn an_unknown_leg_is_an_error_not_a_silent_drop() {
        // Falling back to `none` on a typo would turn a guarded origin into an
        // open one, on an origin where every caller gets RCE.
        let e = parse_auth("bearer,paswrod").unwrap_err();
        assert!(e.contains("paswrod"));
        assert!(parse_auth("nonsense").is_err());
    }

    #[test]
    fn none_composed_with_a_real_guard_is_rejected() {
        // `none,bearer` reads as "no auth AND bearer", which is either a typo
        // or a misunderstanding. Neither should silently become one of them.
        assert!(parse_auth("none,bearer").is_err());
    }

    #[test]
    fn bearer_without_a_token_is_a_boot_error() {
        let cfg =
            AuthConfig::build(vec![GuardKind::Bearer], None, "X-Forwarded-Email".into(), vec![]);
        assert!(cfg.is_err());
    }

    #[test]
    fn trusted_proxy_without_an_allowlist_is_a_boot_error() {
        // Accepting an identity header from anywhere is the whole vulnerability
        // the allowlist exists to prevent.
        let cfg = AuthConfig::build(
            vec![GuardKind::TrustedProxy],
            None,
            "X-Forwarded-Email".into(),
            vec![],
        );
        assert!(cfg.is_err());
    }
}

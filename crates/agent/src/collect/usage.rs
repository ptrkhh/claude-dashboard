//! Claude subscription usage limits — the same numbers `claude /usage` shows.
//!
//! They come from the OAuth-only endpoint `GET /api/oauth/usage`, authenticated
//! with the subscription token Claude Code stores in `~/.claude/.credentials.json`.
//! Response shape: `{ five_hour: {utilization, resets_at}, seven_day: {...},
//! seven_day_<model>: {...}, ... }` where `utilization` is a 0–100 percentage.
//!
//! Port of `lib/usage.js` and the `claudeUsage` cache in `lib/collect.js`.

use crate::host::log::LogBuffer;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const OAUTH_BETA: &str = "oauth-2025-04-20";
const USER_AGENT: &str = concat!("cdash-agent/", env!("CARGO_PKG_VERSION"));
const DEFAULT_BASE: &str = "https://api.anthropic.com";

/// Time-box on the lookup, so a stalled API can never hold a refresh open.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// How stale the cached limits may get. The strip is a status readout, not a
/// billing ledger. The endpoint rate-limits hard — a request a minute drew a 429
/// every few minutes, with no usable `Retry-After` — so ask rarely.
pub const USAGE_TTL: Duration = Duration::from_secs(300);

/// How long the last good reading is still shown once refreshes start failing.
/// Past this the tiles go away: an expired token or a logout would otherwise
/// freeze them, reset times long gone, until the agent restarts. Long enough to
/// ride out one backed-off retry.
pub const USAGE_MAX_AGE: Duration = Duration::from_secs(1200);

/// Each consecutive failed request doubles the wait before the next one, up to
/// `USAGE_TTL << MAX_BACKOFF` (20 min) — hammering a rate-limited endpoint at
/// the TTL only prolongs that.
const MAX_BACKOFF: u32 = 2;

/// The subscription token goes wherever this points, so `ANTHROPIC_BASE_URL` is
/// honoured only for https or a loopback http (a local proxy or test double).
/// Anything else falls back to the real API rather than sending the token in
/// clear text or to a host the URL merely resembles.
fn base_url_from(raw: Option<&str>) -> String {
    let trusted = |u: &&str| match reqwest::Url::parse(u) {
        Ok(url) => match url.scheme() {
            "https" => true,
            "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
            _ => false,
        },
        Err(_) => false,
    };
    raw.map(|r| r.trim_end_matches('/'))
        .filter(trusted)
        .unwrap_or(DEFAULT_BASE)
        .to_string()
}

fn base_url() -> String {
    base_url_from(std::env::var("ANTHROPIC_BASE_URL").ok().as_deref())
}

/// One limit tile. `short` is the stat-tile label ("Session", "Week", a model);
/// `long` is the tooltip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageLimit {
    pub key: String,
    pub short: String,
    pub long: String,
    pub pct: u32,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<String>,
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn labels_for(key: &str) -> (String, String) {
    match key {
        "five_hour" => ("Session".to_string(), "Current session".to_string()),
        "seven_day" => ("Week".to_string(), "Current week (all models)".to_string()),
        _ => {
            if let Some(model) = key.strip_prefix("seven_day_") {
                let m = cap(model);
                (m.clone(), format!("Current week ({m})"))
            } else if let Some(model) = key.strip_prefix("five_hour_") {
                let m = cap(model);
                (format!("Session {m}"), format!("Current session ({m})"))
            } else {
                (key.to_string(), key.to_string())
            }
        }
    }
}

/// Session first, weekly-all-models next, model-specific weeklies after.
const ORDER: &[&str] = &["five_hour", "seven_day"];
fn rank(key: &str) -> usize {
    ORDER.iter().position(|k| *k == key).unwrap_or(ORDER.len())
}

/// Normalize the raw `/api/oauth/usage` body into an ordered list of limit
/// tiles. Ignores anything that isn't a `{ utilization: number }` bucket, so
/// unknown future fields (metadata, new bucket types) never break the strip.
///
/// The tiebreak is a byte comparison where Node used `localeCompare`; every
/// key the endpoint emits is ASCII, where the two orders agree.
pub fn parse_usage(data: &serde_json::Value) -> Vec<UsageLimit> {
    let Some(obj) = data.as_object() else { return Vec::new() };

    let mut out: Vec<UsageLimit> = obj
        .iter()
        .filter_map(|(key, v)| {
            let util = v.as_object()?.get("utilization")?.as_f64()?;
            let (short, long) = labels_for(key);
            Some(UsageLimit {
                key: key.clone(),
                short,
                long,
                pct: util.round().clamp(0.0, 100.0) as u32,
                resets_at: v.get("resets_at").and_then(|r| r.as_str()).map(String::from),
            })
        })
        .collect();

    out.sort_by(|a, b| rank(&a.key).cmp(&rank(&b.key)).then_with(|| a.key.cmp(&b.key)));
    out
}

/// The subscription token, or `None` for API-key users / logged-out / expired
/// tokens — in which case we show no Claude tiles rather than 401ing.
async fn oauth_token(claude_dir: &Path) -> Option<String> {
    let txt = tokio::fs::read_to_string(claude_dir.join(".credentials.json")).await.ok()?;
    let oauth = serde_json::from_str::<serde_json::Value>(&txt).ok()?;
    let oauth = oauth.get("claudeAiOauth")?;

    // Stale — let the CLI refresh it rather than spending a round trip on a
    // token the API will reject.
    if let Some(exp) = oauth.get("expiresAt").and_then(|e| e.as_f64()) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        if now_ms > exp {
            return None;
        }
    }
    oauth.get("accessToken").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(String::from)
}

/// Why a lookup came back empty. Only `Request` cost a round trip to the API,
/// so only it backs the next attempt off; `NoToken` is a local file read.
#[derive(Debug, PartialEq)]
pub enum Miss {
    NoToken,
    Request(String),
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::NoToken => f.write_str("no usable subscription token"),
            Miss::Request(why) => f.write_str(why),
        }
    }
}

/// Fetch and parse the live limits. The tiles are optional, so the cache folds
/// every `Miss` into "no tiles" and never lets it colour the sessions payload.
pub async fn fetch_usage(claude_dir: &Path) -> Result<Vec<UsageLimit>, Miss> {
    let token = oauth_token(claude_dir).await.ok_or(Miss::NoToken)?;
    let failed = |e: reqwest::Error| Miss::Request(e.without_url().to_string());
    let resp = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(failed)?
        .get(format!("{}/api/oauth/usage", base_url()))
        .bearer_auth(token)
        .header("anthropic-beta", OAUTH_BETA)
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(failed)?;
    if !resp.status().is_success() {
        return Err(Miss::Request(format!("http {}", resp.status().as_u16())));
    }
    resp.json::<serde_json::Value>().await.map(|v| parse_usage(&v)).map_err(failed)
}

#[derive(Default)]
struct UsageState {
    data: Option<Vec<UsageLimit>>,
    /// Last successful refresh; `data` is only shown while this is recent.
    ok: Option<Instant>,
    /// Last attempt, success or not.
    fetched: Option<Instant>,
    /// Consecutive failed requests, capped at `MAX_BACKOFF`.
    fails: u32,
    /// An outage has been logged and not yet recovered from.
    failing: bool,
    busy: bool,
}

impl UsageState {
    /// Time for another lookup. `busy` is what keeps a slow fetch from being
    /// re-entered once per poll: without it a 5-second lookup and a 4-second
    /// poll stack refreshes.
    fn due(&self, now: Instant) -> bool {
        !self.busy
            && self.fetched.is_none_or(|t| now.duration_since(t) > USAGE_TTL * 2u32.pow(self.fails))
    }

    fn shown(&self, now: Instant) -> Option<Vec<UsageLimit>> {
        self.ok.filter(|t| now.duration_since(*t) < USAGE_MAX_AGE)?;
        self.data.clone()
    }

    /// Fold in a finished lookup. A failure still stamps `fetched`, so a
    /// signed-out user retries on the TTL rather than on every poll. The return
    /// value is the log line, if one is due: once per outage, and only once the
    /// outage has cost the tiles. A failure the last good reading still covers
    /// is the endpoint rate-limiting us, not news — logging each one filled the
    /// panel with a "429" line every few minutes while the tiles stayed up.
    fn record(&mut self, res: Result<Vec<UsageLimit>, Miss>, now: Instant) -> Option<String> {
        self.busy = false;
        self.fetched = Some(now);
        match res {
            Ok(data) => {
                (self.data, self.ok, self.fails, self.failing) = (Some(data), Some(now), 0, false);
                None
            }
            Err(miss) => {
                self.fails = match miss {
                    Miss::Request(_) => (self.fails + 1).min(MAX_BACKOFF),
                    Miss::NoToken => 0,
                };
                (self.shown(now).is_none() && !std::mem::replace(&mut self.failing, true))
                    .then(|| format!("claude usage unavailable: {miss}"))
            }
        }
    }
}

/// The limits, refreshed in the background so a 4-second poll never waits on
/// the network. The first poll returns `None`; a transient failure keeps the
/// last good value for `USAGE_MAX_AGE` rather than blanking the tiles.
pub struct UsageCache {
    state: Mutex<UsageState>,
}

impl UsageCache {
    pub fn new() -> Self {
        Self { state: Mutex::new(UsageState::default()) }
    }

    /// Return what is cached, kicking off a refresh when it is due.
    /// Returns immediately either way.
    pub fn get(self: &Arc<Self>, claude_dir: &Path, log: &Arc<LogBuffer>) -> Option<Vec<UsageLimit>> {
        let now = Instant::now();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.due(now) {
            st.busy = true;
            self.clone().spawn_refresh(claude_dir.to_path_buf(), Arc::clone(log));
        }
        st.shown(now)
    }

    fn spawn_refresh(self: Arc<Self>, claude_dir: PathBuf, log: Arc<LogBuffer>) {
        tokio::spawn(async move {
            // FETCH_TIMEOUT bounds the request, not the credentials read before
            // it; a stalled mount would otherwise leave `busy` set for good.
            let res = tokio::time::timeout(FETCH_TIMEOUT * 2, fetch_usage(&claude_dir))
                .await
                .unwrap_or_else(|_| Err(Miss::Request("timed out".to_string())));
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(line) = st.record(res, Instant::now()) {
                log.push(line);
            }
        });
    }
}

impl Default for UsageCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys(v: &[UsageLimit]) -> Vec<&str> {
        v.iter().map(|u| u.key.as_str()).collect()
    }

    #[test]
    fn maps_known_buckets_with_labels_and_reset_times() {
        let out = parse_usage(&json!({
            "five_hour": { "utilization": 66, "resets_at": "2026-07-27T10:59:00Z" },
            "seven_day": { "utilization": 77, "resets_at": "2026-07-29T04:59:00Z" },
        }));
        assert_eq!(
            out,
            vec![
                UsageLimit {
                    key: "five_hour".into(),
                    short: "Session".into(),
                    long: "Current session".into(),
                    pct: 66,
                    resets_at: Some("2026-07-27T10:59:00Z".into()),
                },
                UsageLimit {
                    key: "seven_day".into(),
                    short: "Week".into(),
                    long: "Current week (all models)".into(),
                    pct: 77,
                    resets_at: Some("2026-07-29T04:59:00Z".into()),
                },
            ]
        );
    }

    #[test]
    fn labels_model_specific_buckets_by_model_name() {
        let out = parse_usage(&json!({
            "seven_day_fable": { "utilization": 26, "resets_at": "2026-07-29T05:00:00Z" },
            "seven_day_opus": { "utilization": 10, "resets_at": null },
            "five_hour_opus": { "utilization": 3 },
        }));
        let seen: Vec<(&str, &str)> =
            out.iter().map(|u| (u.short.as_str(), u.long.as_str())).collect();
        assert_eq!(
            seen,
            vec![
                ("Session Opus", "Current session (Opus)"),
                ("Fable", "Current week (Fable)"),
                ("Opus", "Current week (Opus)"),
            ]
        );
        assert_eq!(out[2].resets_at, None, "a null reset time is absent, not the string \"null\"");
    }

    #[test]
    fn orders_session_first_then_weekly_all_then_model_weeklies() {
        let out = parse_usage(&json!({
            "seven_day_sonnet": { "utilization": 5 },
            "seven_day": { "utilization": 50 },
            "five_hour": { "utilization": 20 },
        }));
        assert_eq!(keys(&out), ["five_hour", "seven_day", "seven_day_sonnet"]);
    }

    #[test]
    fn clamps_and_rounds_utilization_to_0_100() {
        let out = parse_usage(&json!({
            "five_hour": { "utilization": 66.7 },
            "seven_day": { "utilization": 140 },
            "seven_day_opus": { "utilization": -3 },
        }));
        assert_eq!(out.iter().map(|u| u.pct).collect::<Vec<_>>(), [67, 100, 0]);
    }

    #[test]
    fn ignores_non_bucket_fields_and_bad_input() {
        assert!(parse_usage(&serde_json::Value::Null).is_empty());
        assert!(parse_usage(&json!("nope")).is_empty());
        assert!(parse_usage(&json!({})).is_empty());
        let out = parse_usage(&json!({
            "five_hour": { "utilization": 10 },
            "note": "hi",
            "extra": { "foo": 1 },
            "stringy": { "utilization": "80" },
        }));
        assert_eq!(keys(&out), ["five_hour"], "only numeric utilization buckets become tiles");
    }

    fn credfile(tag: &str, body: serde_json::Value) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-usage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".credentials.json"), body.to_string()).unwrap();
        dir
    }

    #[tokio::test]
    async fn no_token_means_no_tiles_rather_than_a_failed_request() {
        // API-key users, logged-out users, and an expired subscription token
        // all take this path: no Claude tiles, no error, no request.
        let far_past = 1_000_000_000_000u64;
        for (tag, body) in [
            ("apikey", json!({ "other": true })),
            ("expired", json!({ "claudeAiOauth": { "accessToken": "t", "expiresAt": far_past } })),
            ("empty", json!({ "claudeAiOauth": { "accessToken": "" } })),
        ] {
            assert!(oauth_token(&credfile(tag, body)).await.is_none(), "{tag}");
        }
        assert!(oauth_token(Path::new("/no/such/cdash-dir")).await.is_none());
    }

    #[tokio::test]
    async fn a_live_token_is_read_back() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 3_600_000;
        let dir = credfile(
            "live",
            json!({ "claudeAiOauth": { "accessToken": "sk-tok", "expiresAt": future } }),
        );
        assert_eq!(oauth_token(&dir).await.as_deref(), Some("sk-tok"));
    }

    #[tokio::test]
    async fn the_cache_answers_immediately_and_refreshes_behind_the_poll() {
        // The contract the 4s poll depends on: `get` never awaits the network.
        let cache = Arc::new(UsageCache::new());
        let log = Arc::new(LogBuffer::new());
        let dir = credfile("cache", json!({ "nothing": true }));

        let started = Instant::now();
        assert_eq!(cache.get(&dir, &log), None, "the first poll has nothing yet");
        assert!(started.elapsed() < Duration::from_secs(1), "get must not block on the fetch");

        // Second call while the refresh is in flight must not queue another.
        assert_eq!(cache.get(&dir, &log), None);
        let st = cache.state.lock().unwrap();
        assert!(st.busy || st.fetched.is_some());
    }

    fn reading(pct: u32) -> Vec<UsageLimit> {
        parse_usage(&json!({ "five_hour": { "utilization": pct } }))
    }

    fn req_err() -> Result<Vec<UsageLimit>, Miss> {
        Err(Miss::Request("http 429".into()))
    }

    #[test]
    fn a_failing_refresh_keeps_the_last_reading_only_until_max_age() {
        let t0 = Instant::now();
        let mut st = UsageState::default();
        assert_eq!(st.shown(t0), None, "nothing to show before the first success");
        st.record(Ok(reading(40)), t0);
        assert_eq!(st.shown(t0), Some(reading(40)));

        // The token dies: every later refresh fails, the old numbers ride along…
        st.record(req_err(), t0 + USAGE_TTL * 2);
        assert_eq!(st.shown(t0 + USAGE_MAX_AGE - Duration::from_secs(1)), Some(reading(40)));
        // …but not forever.
        assert_eq!(st.shown(t0 + USAGE_MAX_AGE), None);

        st.record(Ok(reading(55)), t0 + USAGE_MAX_AGE);
        assert_eq!(st.shown(t0 + USAGE_MAX_AGE), Some(reading(55)), "a success revives the tiles");
    }

    #[test]
    fn failed_requests_back_off_and_a_success_resets_it() {
        let t0 = Instant::now();
        let s = Duration::from_secs(1);
        let mut st = UsageState::default();
        assert!(st.due(t0), "the first poll looks it up");

        for (n, wait) in [(1u32, 2), (2, 4), (3, 4), (4, 4)] {
            st.record(req_err(), t0);
            assert!(!st.due(t0 + USAGE_TTL * wait), "after {n} failures wait over {wait}x TTL");
            assert!(st.due(t0 + USAGE_TTL * wait + s), "after {n} failures due past {wait}x TTL");
        }

        st.record(Ok(reading(1)), t0);
        assert!(!st.due(t0 + USAGE_TTL));
        assert!(st.due(t0 + USAGE_TTL + s), "a success is back on the plain TTL");
    }

    #[test]
    fn a_missing_token_retries_on_the_plain_ttl() {
        // No request was made, so there is nothing to back off from — and a user
        // who signs in should see the tiles within a minute, not sixteen.
        let t0 = Instant::now();
        let mut st = UsageState::default();
        for _ in 0..6 {
            st.record(Err(Miss::NoToken), t0);
        }
        assert!(st.due(t0 + USAGE_TTL + Duration::from_secs(1)));
    }

    #[test]
    fn an_in_flight_lookup_is_never_doubled() {
        let t0 = Instant::now();
        let st = UsageState { busy: true, ..Default::default() };
        assert!(!st.due(t0 + USAGE_TTL * 100));
    }

    #[test]
    fn an_outage_is_logged_once_it_costs_the_tiles_and_again_after_it_recovers() {
        let t0 = Instant::now();
        let mut st = UsageState::default();
        // Nothing was ever shown, so the first failure is news — once.
        assert_eq!(st.record(req_err(), t0).as_deref(), Some("claude usage unavailable: http 429"));
        assert_eq!(st.record(req_err(), t0), None, "same outage, no second line");
        assert_eq!(st.record(Ok(reading(1)), t0), None);

        // A rate-limit blip the old reading still covers is silent, however
        // often it flaps — this is the 429 line that used to repeat every few minutes.
        let mut t = t0;
        for _ in 0..3 {
            t += USAGE_TTL;
            assert_eq!(st.record(req_err(), t), None, "the tiles are still up");
            t += USAGE_TTL;
            assert_eq!(st.record(Ok(reading(1)), t), None);
        }

        // A full MAX_AGE with no success: the tiles are gone, so it is news again.
        assert!(st.record(req_err(), t + USAGE_MAX_AGE).is_some());
        assert_eq!(st.record(req_err(), t + USAGE_MAX_AGE), None, "once per outage");
    }

    #[test]
    fn the_token_only_goes_to_https_or_loopback() {
        let real = "https://api.anthropic.com";
        for ok in [
            "https://proxy.internal/base",
            "http://localhost:9999",
            "http://127.0.0.1:9999/",
            "http://[::1]:9999",
        ] {
            assert_eq!(base_url_from(Some(ok)), ok.trim_end_matches('/'), "{ok}");
        }
        for bad in [
            "http://proxy.internal",
            "http://localhost.evil.com",
            "http://127.0.0.1:80@evil.com",
            "ftp://127.0.0.1",
            "not a url",
            "",
        ] {
            assert_eq!(base_url_from(Some(bad)), real, "{bad:?}");
        }
        assert_eq!(base_url_from(None), real);
    }
}

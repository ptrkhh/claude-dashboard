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

/// Time-box on the lookup, so a stalled API can never hold a refresh open.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// How stale the cached limits may get. The strip is a status readout, not a
/// billing ledger; a minute-old percentage is the right trade against a
/// per-poll round trip to the API.
pub const USAGE_TTL: Duration = Duration::from_secs(60);

fn base_url() -> String {
    let raw = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
    raw.trim_end_matches('/').to_string()
}

/// One limit tile. `short` is the stat-tile label (shown as "Claude <short>");
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

/// Fetch and parse the live limits, or `None` if unavailable (no token,
/// network error, non-2xx). Never an error the caller must handle: the tiles
/// are optional, and a failed lookup must not colour the sessions payload.
pub async fn fetch_usage(claude_dir: &Path) -> Option<Vec<UsageLimit>> {
    let token = oauth_token(claude_dir).await?;
    let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build().ok()?;
    let resp = client
        .get(format!("{}/api/oauth/usage", base_url()))
        .bearer_auth(token)
        .header("anthropic-beta", OAUTH_BETA)
        .header("Content-Type", "application/json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    Some(parse_usage(&resp.json::<serde_json::Value>().await.ok()?))
}

struct UsageState {
    data: Option<Vec<UsageLimit>>,
    fetched: Option<Instant>,
    busy: bool,
}

/// The limits, refreshed in the background so a 4-second poll never waits on
/// the network. The first poll returns `None`; a transient failure keeps the
/// last good value rather than blanking the tiles.
pub struct UsageCache {
    state: Mutex<UsageState>,
}

impl UsageCache {
    pub fn new() -> Self {
        Self { state: Mutex::new(UsageState { data: None, fetched: None, busy: false }) }
    }

    /// Return what is cached, kicking off a refresh when it has gone stale.
    /// Returns immediately either way.
    ///
    /// `busy` is what keeps a slow fetch from being re-entered once per poll:
    /// without it a 5-second lookup and a 4-second poll stack refreshes.
    pub fn get(self: &Arc<Self>, claude_dir: &Path, log: &Arc<LogBuffer>) -> Option<Vec<UsageLimit>> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let stale = st.fetched.is_none_or(|t| t.elapsed() > USAGE_TTL);
        if stale && !st.busy {
            st.busy = true;
            self.clone().spawn_refresh(claude_dir.to_path_buf(), Arc::clone(log));
        }
        st.data.clone()
    }

    fn spawn_refresh(self: Arc<Self>, claude_dir: PathBuf, log: Arc<LogBuffer>) {
        tokio::spawn(async move {
            let fresh = fetch_usage(&claude_dir).await;
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // A failed lookup still stamps `fetched`, so a signed-out user
            // retries on the TTL rather than on every poll — and says so once
            // rather than filing a line every minute for the process's life.
            if fresh.is_some() {
                st.data = fresh;
            } else if st.data.is_none() && st.fetched.is_none() {
                log.push("claude usage unavailable");
            }
            st.fetched = Some(Instant::now());
            st.busy = false;
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
        assert!(cache.state.lock().unwrap().busy || cache.state.lock().unwrap().fetched.is_some());
    }
}

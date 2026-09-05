use super::cache::mtime_ms;
use super::fsio::read_if;
use crate::parse::paths::project_dir_name;
use crate::parse::transcript::parse_rc_file;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// `~/.claude/sessions/<pid>.json` — the authoritative link between a pane's
/// pid and its session id, so the transcript never has to be guessed.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionFile {
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub entrypoint: Option<String>,
    /// Epoch milliseconds. `f64` for the same reason `ts` is: the field is
    /// JSON-numeric and a wrongly-typed value must not drop the whole entry.
    #[serde(rename = "startedAt")]
    pub started_at: Option<f64>,
    #[serde(rename = "bridgeSessionId")]
    pub bridge_session_id: Option<String>,
}

fn session_path(claude_dir: &Path, pid: i32) -> PathBuf {
    claude_dir.join("sessions").join(format!("{pid}.json"))
}

pub async fn session_file_for(claude_dir: &Path, pid: i32) -> Option<SessionFile> {
    let txt = read_if(&session_path(claude_dir, pid)).await?;
    serde_json::from_str(&txt).ok()
}

/// The "Open in Claude" link a session-file body carries, if any. The one
/// construction both locators share: `parse_rc_file` stringifies a numeric or
/// boolean id rather than dropping the link along with it.
pub fn rc_link_from(txt: &str) -> Option<String> {
    parse_rc_file(txt).map(|id| format!("https://claude.ai/code/{id}"))
}

pub async fn rc_link_for(claude_dir: &Path, pid: i32) -> Option<String> {
    rc_link_from(&read_if(&session_path(claude_dir, pid)).await?)
}

/// The newest `.jsonl` in the project directory modified at or after session
/// start, with Node's 5-second grace (`lib/collect.js:101`) — a transcript
/// written just before the pane appeared still belongs to it.
pub async fn transcript_for(
    claude_dir: &Path,
    cwd: &str,
    created_sec: i64,
) -> Option<(PathBuf, f64)> {
    let dir = claude_dir.join("projects").join(project_dir_name(cwd));
    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    let mut best: Option<(PathBuf, f64)> = None;
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(md) = e.metadata().await else { continue };
        let ms = mtime_ms(&md);
        if ms / 1000.0 < (created_sec - 5) as f64 {
            continue;
        }
        if best.as_ref().is_none_or(|(_, b)| ms > *b) {
            best = Some((p, ms));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-lookup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    #[tokio::test]
    async fn session_file_reads_the_fields_collect_depends_on() {
        let d = claude_dir("sess");
        std::fs::write(
            d.join("sessions/4242.json"),
            r#"{"sessionId":"abc","cwd":"/x","name":"api","entrypoint":"cli","startedAt":1700000000000,"bridgeSessionId":"session_z"}"#,
        )
        .unwrap();
        let s = session_file_for(&d, 4242).await.unwrap();
        assert_eq!(s.session_id.as_deref(), Some("abc"));
        assert_eq!(s.cwd.as_deref(), Some("/x"));
        assert_eq!(s.name.as_deref(), Some("api"));
        assert_eq!(s.entrypoint.as_deref(), Some("cli"));
        assert_eq!(s.started_at, Some(1_700_000_000_000.0));
    }

    #[tokio::test]
    async fn a_missing_or_malformed_session_file_is_none_not_an_error() {
        let d = claude_dir("bad");
        assert!(session_file_for(&d, 1).await.is_none());
        std::fs::write(d.join("sessions/2.json"), "not json").unwrap();
        assert!(session_file_for(&d, 2).await.is_none());
    }

    #[tokio::test]
    async fn rc_link_is_built_from_the_bridge_session_id() {
        let d = claude_dir("rc");
        std::fs::write(d.join("sessions/7.json"), r#"{"bridgeSessionId":"session_abc"}"#).unwrap();
        assert_eq!(
            rc_link_for(&d, 7).await.as_deref(),
            Some("https://claude.ai/code/session_abc")
        );
        std::fs::write(d.join("sessions/8.json"), r#"{"other":1}"#).unwrap();
        assert_eq!(rc_link_for(&d, 8).await, None);
    }

    #[tokio::test]
    async fn transcript_for_takes_the_newest_jsonl_at_or_after_session_start() {
        let d = claude_dir("tr");
        let proj = d.join("projects").join("-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("old.jsonl"), "{}").unwrap();
        std::fs::write(proj.join("new.jsonl"), "{}").unwrap();
        std::fs::write(proj.join("notes.txt"), "ignored").unwrap();

        // now-ish start time; both files were just written so both qualify.
        let now_sec = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (file, _) = transcript_for(&d, "/x", now_sec).await.unwrap();
        assert_eq!(file.extension().unwrap(), "jsonl", "non-jsonl files are not candidates");
    }

    #[tokio::test]
    async fn a_transcript_older_than_the_five_second_grace_is_rejected() {
        // E9: a stale transcript from a previous session in the same directory
        // must not be attributed to a pane that started later.
        let d = claude_dir("grace");
        let proj = d.join("projects").join("-y");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("stale.jsonl"), "{}").unwrap();

        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        assert!(transcript_for(&d, "/y", far_future).await.is_none());
    }

    #[tokio::test]
    async fn a_missing_project_directory_is_none() {
        let d = claude_dir("noproj");
        assert!(transcript_for(&d, "/nope", 0).await.is_none());
    }
}

use super::ctx::Ctx;
use super::fsio::read_tail;
use super::lookup::session_file_for;
use crate::host::proc::ProcRow;
use crate::parse::git::{parse_git_status, GitStatus};
use crate::parse::paths::project_dir_name;
use crate::parse::transcript::parse_transcript;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// One entry of `/api/sessions` `running[]`. Field names and shapes are the
/// parity gate's contract — see the exemption list in the spec before changing
/// any of them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub name: String,
    pub dir: String,
    pub pid: i32,
    #[serde(rename = "uptimeSec")]
    pub uptime_sec: i64,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(rename = "rcLink")]
    pub rc_link: Option<String>,
    pub git: Option<GitStatus>,
    pub working: bool,
    #[serde(rename = "lastMessage")]
    pub last_message: Option<String>,
    pub sid: Option<String>,
    pub cpu: Option<f32>,
    #[serde(rename = "rssKb")]
    pub rss_kb: u64,
    /// Rust-only; Node has no such field. Exempted by name in the parity gate.
    #[serde(rename = "cpuSampleAgeMs")]
    pub cpu_sample_age_ms: u128,
    /// Node omitted this key entirely for pane sessions and set it to `true`
    /// for external ones. `skip_serializing_if` reproduces that exactly.
    #[serde(skip_serializing_if = "is_false")]
    pub external: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// Sessions this dashboard did not launch: every `~/.claude/sessions/<pid>.json`
/// whose pid is still alive. Read-only — they live in terminals we do not own.
pub async fn external_sessions(
    ctx: &Arc<Ctx>,
    rows: &[ProcRow],
    pane_pids: &HashSet<i32>,
    now_ms: f64,
) -> Vec<Session> {
    let alive: HashSet<i32> = rows.iter().map(|r| r.pid).collect();
    let dir = ctx.claude_dir.join("sessions");
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return Vec::new();
    };

    let mut out = Vec::new();
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(pid) = p
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pane_pids.contains(&pid) || !alive.contains(&pid) {
            continue;
        }
        let Some(sess) = session_file_for(&ctx.claude_dir, pid).await else { continue };
        let (Some(sid), Some(cwd)) = (sess.session_id.clone(), sess.cwd.clone()) else { continue };
        // E1: 'cli' is a session someone is sitting in front of; 'sdk-cli' is
        // programmatic and not ours to show.
        if sess.entrypoint.as_deref() != Some("cli") {
            continue;
        }

        let file = ctx
            .claude_dir
            .join("projects")
            .join(project_dir_name(&cwd))
            .join(format!("{sid}.jsonl"));
        let md = tokio::fs::metadata(&file).await.ok();
        let mut last_message = None;
        if md.is_some() {
            if let Some(txt) = read_tail(&file).await {
                last_message = parse_transcript(&txt).last_assistant_text;
            }
        }
        let working = md
            .as_ref()
            .map(|m| now_ms - super::cache::mtime_ms(m) < 10_000.0)
            .unwrap_or(false);

        let usage = {
            let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
            s.tree_usage(pid)
        };
        let git_out =
            Arc::clone(&ctx.git).status_for(Arc::clone(&ctx.runner), &cwd, now_ms as u64);

        out.push(Session {
            name: sess
                .name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| basename(&cwd)),
            dir: cwd,
            pid,
            uptime_sec: sess
                .started_at
                .map(|t| (((now_ms - t) / 1000.0).round() as i64).max(0))
                .unwrap_or(0),
            model: None,
            effort: None,
            rc_link: sess
                .bridge_session_id
                .map(|id| format!("https://claude.ai/code/{id}")),
            git: git_out.as_deref().map(parse_git_status),
            working,
            last_message,
            sid: Some(sid),
            cpu: usage.cpu,
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: usage.cpu_sample_age_ms,
            external: true,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;
    use std::path::PathBuf;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-ext-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    fn ctx_for(claude_dir: PathBuf) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let host = crate::host::init::Host {
            runner: crate::host::cmd::Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, claude_dir, None))
    }

    fn write_session(dir: &Path, pid: i32, body: &str) {
        std::fs::write(dir.join("sessions").join(format!("{pid}.json")), body).unwrap();
    }

    fn rows(pids: &[i32]) -> Vec<ProcRow> {
        pids.iter()
            .map(|p| ProcRow { pid: *p, ppid: 1, cpu: 1.0, rss_kb: 100 })
            .collect()
    }

    const CLI: &str = r#"{"sessionId":"s-1","cwd":"/proj","entrypoint":"cli","startedAt":1000,"name":"api"}"#;

    #[tokio::test]
    async fn a_live_cli_session_is_reported() {
        let d = tempdir("live");
        write_session(&d, 500, CLI);
        let out = external_sessions(&ctx_for(d), &rows(&[500]), &HashSet::new(), 61_000.0).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "api");
        assert_eq!(out[0].dir, "/proj");
        assert_eq!(out[0].sid.as_deref(), Some("s-1"));
        assert!(out[0].external);
        assert_eq!(out[0].uptime_sec, 60, "(61000 - 1000) / 1000");
    }

    #[tokio::test]
    async fn a_name_less_session_falls_back_to_the_directory_basename() {
        let d = tempdir("noname");
        write_session(&d, 501, r#"{"sessionId":"s","cwd":"/a/proj","entrypoint":"cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[501]), &HashSet::new(), 0.0).await;
        assert_eq!(out[0].name, "proj");
        assert_eq!(out[0].uptime_sec, 0, "no startedAt means zero, not a negative age");
    }

    #[tokio::test]
    async fn an_sdk_cli_session_is_excluded() {
        // E1: claude-mem observers and SDK runs are not sessions anyone is
        // sitting in front of. Showing them makes the list untrustworthy.
        let d = tempdir("sdk");
        write_session(&d, 502, r#"{"sessionId":"s","cwd":"/p","entrypoint":"sdk-cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[502]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_session_missing_its_entrypoint_is_excluded() {
        let d = tempdir("noentry");
        write_session(&d, 503, r#"{"sessionId":"s","cwd":"/p"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[503]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_dead_pid_is_excluded() {
        // E4: the session file outlives the process that wrote it.
        let d = tempdir("dead");
        write_session(&d, 504, CLI);
        let out = external_sessions(&ctx_for(d), &rows(&[999]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_pid_already_shown_as_a_pane_is_excluded() {
        // E3: without this the same session appears twice, once per source.
        let d = tempdir("dupe");
        write_session(&d, 505, CLI);
        let panes: HashSet<i32> = [505].into_iter().collect();
        let out = external_sessions(&ctx_for(d), &rows(&[505]), &panes, 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_session_without_session_id_or_cwd_is_excluded() {
        let d = tempdir("partial");
        write_session(&d, 506, r#"{"cwd":"/p","entrypoint":"cli"}"#);
        write_session(&d, 507, r#"{"sessionId":"s","entrypoint":"cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[506, 507]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn non_json_files_and_non_numeric_names_are_skipped() {
        // E11: the sessions directory is not guaranteed to hold only pid files.
        let d = tempdir("junk");
        std::fs::write(d.join("sessions/notes.txt"), "x").unwrap();
        std::fs::write(d.join("sessions/abc.json"), CLI).unwrap();
        let out = external_sessions(&ctx_for(d), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_missing_sessions_directory_yields_an_empty_list() {
        let dir = std::env::temp_dir().join(format!("cdash-ext-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = external_sessions(&ctx_for(dir), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }
}

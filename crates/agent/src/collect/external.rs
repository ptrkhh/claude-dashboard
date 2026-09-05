use super::ctx::{Ctx, Meta};
use super::fsio::read_tail;
use super::lookup::{session_file_for, SessionFile};
use super::side::Side;
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// Every `<claude_dir>/sessions/<pid>.json` whose pid is a live `claude`
/// process with a `cli` entrypoint, minus `exclude`. This is the one liveness
/// predicate: the list uses it to show sessions, kill uses it to refuse a
/// recycled pid. Session files outlive their process.
pub async fn live_session_files(
    side: &Side,
    rows: &[ProcRow],
    exclude: &HashSet<i32>,
) -> Vec<(i32, SessionFile)> {
    let alive: HashSet<i32> = rows
        .iter()
        .filter(|r| r.name == "claude" || r.name == "claude.exe")
        .map(|r| r.pid)
        .collect();
    let dir = side.claude_dir.join("sessions");
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
        if exclude.contains(&pid) || !alive.contains(&pid) {
            continue;
        }
        let Some(sess) = session_file_for(&side.claude_dir, pid).await else { continue };
        // E1: 'cli' is a session someone is sitting in front of; 'sdk-cli' is
        // programmatic and not ours to show.
        if sess.entrypoint.as_deref() != Some("cli") {
            continue;
        }
        out.push((pid, sess));
    }
    out
}

/// Sessions known from their session files: ours when the `name` carries the
/// `cdash-` prefix the launcher sets, external otherwise. On a tmux side the
/// pane pids are excluded first, so a tmux session is never listed twice.
pub async fn file_sessions(
    ctx: &Arc<Ctx>,
    side: &Side,
    rows: &[ProcRow],
    pane_pids: &HashSet<i32>,
    now_ms: f64,
) -> Vec<Session> {
    let mut out = Vec::new();
    for (pid, sess) in live_session_files(side, rows, pane_pids).await {
        let (Some(sid), Some(cwd)) = (sess.session_id.clone(), sess.cwd.clone()) else { continue };
        let name = sess.name.clone().filter(|n| !n.is_empty());
        let ours = name.as_deref().is_some_and(|n| n.starts_with("cdash-"));
        let meta = match &name {
            Some(n) if ours => ctx.meta_get(n).unwrap_or_default(),
            _ => Meta::default(),
        };

        let file = side
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

        let usage = side.tree_usage(&ctx.host.sampler, rows, pid);
        let git_out =
            Arc::clone(&ctx.git).status_for(Arc::clone(&side.runner), &cwd, now_ms as u64);
        let rc_link = meta.rc_link.clone().or_else(|| {
            sess.bridge_session_id
                .clone()
                .map(|id| format!("https://claude.ai/code/{id}"))
        });

        out.push(Session {
            name: name.unwrap_or_else(|| basename(&cwd)),
            dir: cwd,
            pid,
            uptime_sec: sess
                .started_at
                .map(|t| (((now_ms - t) / 1000.0).round() as i64).max(0))
                .unwrap_or(0),
            model: meta.model,
            effort: meta.effort,
            rc_link,
            git: git_out.as_deref().map(parse_git_status),
            working,
            last_message,
            sid: Some(sid),
            cpu: usage.cpu,
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: usage.cpu_sample_age_ms,
            external: !ours,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::ctx::Meta;
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
            .map(|p| ProcRow {
                pid: *p,
                ppid: 1,
                name: "claude".into(),
                cpu: 1.0,
                rss_kb: 100,
            })
            .collect()
    }

    const CLI: &str = r#"{"sessionId":"s-1","cwd":"/proj","entrypoint":"cli","startedAt":1000,"name":"api"}"#;

    #[tokio::test]
    async fn a_live_cli_session_is_reported() {
        let d = tempdir("live");
        write_session(&d, 500, CLI);
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[500]), &HashSet::new(), 61_000.0).await;
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
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[501]), &HashSet::new(), 0.0).await;
        assert_eq!(out[0].name, "proj");
        assert_eq!(out[0].uptime_sec, 0, "no startedAt means zero, not a negative age");
    }

    #[tokio::test]
    async fn an_sdk_cli_session_is_excluded() {
        // E1: claude-mem observers and SDK runs are not sessions anyone is
        // sitting in front of. Showing them makes the list untrustworthy.
        let d = tempdir("sdk");
        write_session(&d, 502, r#"{"sessionId":"s","cwd":"/p","entrypoint":"sdk-cli"}"#);
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[502]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_session_missing_its_entrypoint_is_excluded() {
        let d = tempdir("noentry");
        write_session(&d, 503, r#"{"sessionId":"s","cwd":"/p"}"#);
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[503]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_dead_pid_is_excluded() {
        // E4: the session file outlives the process that wrote it.
        let d = tempdir("dead");
        write_session(&d, 504, CLI);
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[999]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_recycled_pid_belonging_to_another_process_is_excluded() {
        let d = tempdir("recycled");
        write_session(&d, 504, CLI);
        let mut live = rows(&[504]);
        live[0].name = "bash".into();

        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &live, &HashSet::new(), 0.0).await;

        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_pid_already_shown_as_a_pane_is_excluded() {
        // E3: without this the same session appears twice, once per source.
        let d = tempdir("dupe");
        write_session(&d, 505, CLI);
        let panes: HashSet<i32> = [505].into_iter().collect();
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[505]), &panes, 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_session_without_session_id_or_cwd_is_excluded() {
        let d = tempdir("partial");
        write_session(&d, 506, r#"{"cwd":"/p","entrypoint":"cli"}"#);
        write_session(&d, 507, r#"{"sessionId":"s","entrypoint":"cli"}"#);
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[506, 507]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn non_json_files_and_non_numeric_names_are_skipped() {
        // E11: the sessions directory is not guaranteed to hold only pid files.
        let d = tempdir("junk");
        std::fs::write(d.join("sessions/notes.txt"), "x").unwrap();
        std::fs::write(d.join("sessions/abc.json"), CLI).unwrap();
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_missing_sessions_directory_yields_an_empty_list() {
        let dir = std::env::temp_dir().join(format!("cdash-ext-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ctx_for(dir);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_cdash_named_session_is_ours_and_carries_its_meta() {
        // Without tmux there is no session name to match: the `--name` the
        // launcher passes is the only durable mark of a session we started.
        let d = tempdir("ours");
        write_session(
            &d,
            508,
            r#"{"sessionId":"s-8","cwd":"/proj","entrypoint":"cli","name":"cdash-proj-1200-abc","bridgeSessionId":"session_ours"}"#,
        );
        let ctx = ctx_for(d);
        ctx.meta_set(
            "cdash-proj-1200-abc",
            Meta { model: Some("opus".into()), effort: Some("high".into()), rc_link: None },
        );
        let out = file_sessions(&ctx, ctx.native(), &rows(&[508]), &HashSet::new(), 0.0).await;
        assert_eq!(out.len(), 1);
        assert!(!out[0].external, "a cdash- name is a session this dashboard launched");
        assert_eq!(out[0].name, "cdash-proj-1200-abc");
        assert_eq!(out[0].model.as_deref(), Some("opus"));
        assert_eq!(out[0].rc_link.as_deref(), Some("https://claude.ai/code/session_ours"));
    }

    #[tokio::test]
    async fn live_session_files_applies_the_liveness_predicate() {
        // Kill reuses this: a stale file whose pid was recycled must never
        // match, or taskkill would reach a foreign process.
        // Not tempdir("live"): `a_live_cli_session_is_reported` already owns
        // that directory and these tests run in parallel.
        let d = tempdir("liveness");
        write_session(&d, 600, CLI);
        write_session(&d, 601, CLI);
        write_session(&d, 602, r#"{"sessionId":"s","cwd":"/p","entrypoint":"sdk-cli"}"#);
        let ctx = ctx_for(d);
        let mut live = rows(&[600, 602]);
        live.push(ProcRow { pid: 601, ppid: 1, name: "bash".into(), cpu: 0.0, rss_kb: 1 });
        let files = live_session_files(ctx.native(), &live, &HashSet::new()).await;
        let pids: Vec<i32> = files.iter().map(|(p, _)| *p).collect();
        assert_eq!(pids, vec![600], "601 is a recycled pid, 602 is not a cli session");
    }
}

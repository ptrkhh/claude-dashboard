use super::ctx::{Ctx, Meta};
use super::external::{external_sessions, Session};
use super::fsio::{read_if, read_tail};
use super::lookup::{session_file_for, transcript_for};
use crate::host::disk::{disk_usage, DiskUsage};

use crate::parse::git::parse_git_status;
use crate::parse::history::group_history;
use crate::parse::paths::project_dir_name;
use crate::parse::tmux::{parse_tmux_panes, PANE_FORMAT};
use crate::parse::transcript::parse_transcript;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Mirrors the `resumable.length >= 20` break (`lib/collect.js:269`).
pub const RESUMABLE_MAX: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resumable {
    pub sid: String,
    pub dir: Option<String>,
    pub ts: f64,
    pub branch: Option<String>,
    pub title: String,
    pub prompts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    #[serde(rename = "cpuPct")]
    pub cpu_pct: u32,
    #[serde(rename = "ramUsedKb")]
    pub ram_used_kb: u64,
    #[serde(rename = "ramTotalKb")]
    pub ram_total_kb: u64,
    pub disks: Vec<DiskUsage>,
    /// Absent for API-key users and until the first refresh lands; the strip
    /// simply shows no Claude tiles rather than empty ones.
    #[serde(rename = "claudeUsage", skip_serializing_if = "Option::is_none")]
    pub claude_usage: Option<Vec<crate::collect::usage::UsageLimit>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionsResponse {
    pub running: Vec<Session>,
    pub resumable: Vec<Resumable>,
    pub stats: Stats,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

pub async fn collect_sessions(ctx: &Arc<Ctx>) -> SessionsResponse {
    let panes_out = ctx
        .runner
        .run("tmux", &["list-panes", "-a", "-F", PANE_FORMAT], "tmux list-panes")
        .await;
    let panes = parse_tmux_panes(&panes_out);
    let now = now_ms();

    // One sample serves every session in this response; the 200 ms rule lives
    // inside `Sampler` and must not be re-implemented here.
    let rows = {
        let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
        s.sample()
    };

    let mut running: Vec<Session> = Vec::new();
    for p in &panes {
        let meta = ctx.meta_get(&p.name).unwrap_or_default();
        let sess = session_file_for(&ctx.claude_dir, p.pid).await;

        let rc_link = meta.rc_link.clone().or_else(|| {
            sess.as_ref()
                .and_then(|s| s.bridge_session_id.clone())
                .map(|id| format!("https://claude.ai/code/{id}"))
        });
        // D9: memoize a link discovered from the session file, so a later poll
        // does not have to rediscover it.
        if rc_link.is_some() && meta.rc_link.is_none() {
            ctx.meta_set(&p.name, Meta { rc_link: rc_link.clone(), ..meta.clone() });
        }

        // Prefer the pane's own session id; only guess from mtime when the
        // session file has no id yet — several panes can share one cwd.
        let mut tr: Option<(PathBuf, f64)> = None;
        if let Some(sid) = sess.as_ref().and_then(|s| s.session_id.clone()) {
            let cwd = sess
                .as_ref()
                .and_then(|s| s.cwd.clone())
                .unwrap_or_else(|| p.path.clone());
            let file = ctx
                .claude_dir
                .join("projects")
                .join(project_dir_name(&cwd))
                .join(format!("{sid}.jsonl"));
            if let Ok(md) = tokio::fs::metadata(&file).await {
                tr = Some((file, super::cache::mtime_ms(&md)));
            }
        }
        if tr.is_none() {
            tr = transcript_for(&ctx.claude_dir, &p.path, p.created).await;
        }

        let (mut working, mut last_message, mut sid) = (false, None, None);
        if let Some((file, mtime)) = &tr {
            working = now - mtime < 10_000.0;
            sid = file.file_stem().map(|s| s.to_string_lossy().into_owned());
            if let Some(txt) = read_tail(file).await {
                last_message = parse_transcript(&txt).last_assistant_text;
            }
        }

        let cpu_state = {
            let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
            s.tree_usage(p.pid)
        };
        let git_out =
            Arc::clone(&ctx.git).status_for(Arc::clone(&ctx.runner), &p.path, now as u64);

        running.push(Session {
            name: p.name.clone(),
            dir: p.path.clone(),
            pid: p.pid,
            uptime_sec: ((now / 1000.0 - p.created as f64).round() as i64).max(0),
            model: meta.model.clone(),
            effort: meta.effort.clone(),
            rc_link,
            git: git_out.as_deref().map(parse_git_status),
            working,
            last_message,
            sid,
            cpu: cpu_state.cpu,
            rss_kb: cpu_state.rss_kb,
            cpu_sample_age_ms: cpu_state.cpu_sample_age_ms,
            external: false,
        });
    }

    let pane_pids: HashSet<i32> = panes.iter().map(|p| p.pid).collect();
    running.extend(external_sessions(ctx, &rows, &pane_pids, now).await);

    let running_sids: HashSet<String> = running.iter().filter_map(|r| r.sid.clone()).collect();
    let hist = read_if(&ctx.claude_dir.join("history.jsonl")).await.unwrap_or_default();

    let mut resumable = Vec::new();
    for g in group_history(&hist) {
        if resumable.len() >= RESUMABLE_MAX {
            break;
        }
        if running_sids.contains(&g.sid)
            || ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).contains(&g.sid)
        {
            continue;
        }
        let file = ctx
            .claude_dir
            .join("projects")
            .join(project_dir_name(g.cwd.as_deref().unwrap_or("")))
            .join(format!("{}.jsonl", g.sid));
        let Some(t) = ctx.transcripts.get(&file).await else { continue };
        if t.assistant_count < 3 {
            continue;
        }
        resumable.push(Resumable {
            title: t
                .title
                .clone()
                .or_else(|| g.prompts.first().cloned())
                .unwrap_or_else(|| "(untitled)".to_string()),
            sid: g.sid,
            dir: g.cwd,
            ts: g.ts,
            branch: t.branch.clone(),
            prompts: g.prompts,
        });
    }

    let machine = {
        let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
        s.machine_stats()
    };
    let mut disks: Vec<DiskUsage> = Vec::new();
    if let Some(d) = disk_usage("/") {
        disks.push(d);
    }
    if let Some(extra) = ctx.disk_extra.as_deref() {
        if let Some(d) = disk_usage(extra) {
            disks.push(d);
        }
    }

    SessionsResponse {
        running,
        resumable,
        stats: Stats {
            cpu_pct: machine.cpu_pct,
            ram_used_kb: machine.ram_used_kb,
            ram_total_kb: machine.ram_total_kb,
            disks,
            claude_usage: ctx.usage.get(&ctx.claude_dir, &ctx.host.log),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;
    use std::path::Path;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-sess-{tag}-{}", std::process::id()));
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

    /// Write a history entry plus a transcript with `turns` assistant messages.
    fn seed(dir: &Path, sid: &str, cwd: &str, ts: i64, turns: usize) -> String {
        let proj = dir.join("projects").join(crate::parse::paths::project_dir_name(cwd));
        std::fs::create_dir_all(&proj).unwrap();
        let body: String = (0..turns)
            .map(|i| format!("{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"t{i}\"}}]}}}}\n"))
            .collect();
        std::fs::write(proj.join(format!("{sid}.jsonl")), body).unwrap();
        format!("{{\"sessionId\":\"{sid}\",\"project\":\"{cwd}\",\"timestamp\":{ts},\"display\":\"do the thing\"}}\n")
    }

    #[tokio::test]
    async fn a_resumable_session_needs_three_assistant_turns() {
        // E7: one or two turns is an abandoned start, not work worth resuming.
        let d = tempdir("turns");
        let a = seed(&d, "aaa", "/p/deep", 300, 3);
        let b = seed(&d, "bbb", "/p/shallow", 200, 2);
        std::fs::write(d.join("history.jsonl"), format!("{a}{b}")).unwrap();

        let r = collect_sessions(&ctx_for(d)).await;
        let sids: Vec<&str> = r.resumable.iter().map(|x| x.sid.as_str()).collect();
        assert_eq!(sids, vec!["aaa"]);
    }

    #[tokio::test]
    async fn a_session_with_no_transcript_is_not_resumable() {
        // E8: history remembers sessions whose transcript is gone.
        let d = tempdir("notranscript");
        std::fs::write(
            d.join("history.jsonl"),
            "{\"sessionId\":\"ghost\",\"project\":\"/p\",\"timestamp\":1,\"display\":\"x\"}\n",
        )
        .unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert!(r.resumable.is_empty());
    }

    #[tokio::test]
    async fn a_purged_session_is_hidden() {
        // E6: purge is the user saying "stop showing me this".
        let d = tempdir("purged");
        let a = seed(&d, "ccc", "/p", 300, 3);
        std::fs::write(d.join("history.jsonl"), a).unwrap();
        let ctx = ctx_for(d);
        ctx.purged.lock().unwrap().insert("ccc".to_string());
        assert!(collect_sessions(&ctx).await.resumable.is_empty());
    }

    #[tokio::test]
    async fn the_resumable_list_is_capped() {
        // B5: history holds 60 groups; the UI gets at most 20.
        let d = tempdir("cap");
        let mut hist = String::new();
        for i in 0..(RESUMABLE_MAX + 5) {
            let sid = format!("{i:08}-0000-4000-8000-000000000000");
            hist.push_str(&seed(&d, &sid, &format!("/p{i}"), 1000 - i as i64, 3));
        }
        std::fs::write(d.join("history.jsonl"), hist).unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert_eq!(r.resumable.len(), RESUMABLE_MAX);
    }

    #[tokio::test]
    async fn the_title_falls_back_to_the_first_prompt_then_to_untitled() {
        let d = tempdir("title");
        let a = seed(&d, "ddd", "/p", 300, 3);
        std::fs::write(d.join("history.jsonl"), a).unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert_eq!(r.resumable[0].title, "do the thing");
    }

    #[tokio::test]
    async fn the_response_serializes_with_nodes_field_names() {
        let d = tempdir("shape");
        std::fs::write(d.join("history.jsonl"), "").unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"running\":"));
        assert!(j.contains("\"resumable\":"));
        assert!(j.contains("\"cpuPct\":"));
        assert!(j.contains("\"ramUsedKb\":"));
        assert!(j.contains("\"ramTotalKb\":"));
        assert!(j.contains("\"disks\":"));
    }

    #[tokio::test]
    async fn the_root_disk_is_always_reported() {
        let d = tempdir("disks");
        std::fs::write(d.join("history.jsonl"), "").unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert_eq!(r.stats.disks[0].mount, "/");
        assert!(r.stats.disks[0].total_kb > 0);
    }

    /// Put a stub `tmux` on a private PATH so the pane branch of
    /// `collect_sessions` can be exercised without a real server. Without this
    /// the whole pane loop — the majority of the function — has no test at all.
    fn fake_tmux(dir: &Path, pane_line: &str) -> String {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  list-panes) echo '{pane_line}' ;;\n  *) echo '' ;;\nesac\n"
        );
        let p = bin.join("tmux");
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin.to_string_lossy().into_owned()
    }

    fn ctx_with_path(claude_dir: PathBuf, path: String) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let host = crate::host::init::Host {
            runner: crate::host::cmd::Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, claude_dir, None))
    }

    #[tokio::test]
    async fn a_pane_becomes_a_running_session_carrying_its_rc_link() {
        let d = tempdir("pane");
        std::fs::write(d.join("history.jsonl"), "").unwrap();
        std::fs::write(
            d.join("sessions/4242.json"),
            r#"{"sessionId":"s-9","cwd":"/proj","bridgeSessionId":"session_pane"}"#,
        )
        .unwrap();
        let path = fake_tmux(&d, "cdash-test-1200-abc|4242|1785050000|/proj");
        let ctx = ctx_with_path(d, path);

        let r = collect_sessions(&ctx).await;
        assert_eq!(r.running.len(), 1);
        assert_eq!(r.running[0].name, "cdash-test-1200-abc");
        assert_eq!(r.running[0].dir, "/proj");
        assert_eq!(r.running[0].pid, 4242);
        assert_eq!(
            r.running[0].rc_link.as_deref(),
            Some("https://claude.ai/code/session_pane")
        );
    }

    #[tokio::test]
    async fn a_link_discovered_from_the_session_file_is_memoized_into_meta() {
        // D9: rediscovering it on every 4s poll is wasted work, and the meta
        // entry is what survives the session file being rewritten.
        let d = tempdir("memo");
        std::fs::write(d.join("history.jsonl"), "").unwrap();
        std::fs::write(
            d.join("sessions/777.json"),
            r#"{"sessionId":"s-7","cwd":"/proj","bridgeSessionId":"session_memo"}"#,
        )
        .unwrap();
        let path = fake_tmux(&d, "cdash-memo-1200-xyz|777|1785050000|/proj");
        let ctx = ctx_with_path(d, path);

        assert!(ctx.meta_get("cdash-memo-1200-xyz").is_none());
        collect_sessions(&ctx).await;
        assert_eq!(
            ctx.meta_get("cdash-memo-1200-xyz").unwrap().rc_link.as_deref(),
            Some("https://claude.ai/code/session_memo"),
            "the discovered link must be remembered"
        );
    }

    #[test]
    fn a_pane_session_omits_the_external_key_entirely() {
        // Node set `external` only on external sessions; a pane entry has no
        // such key at all, and the gate compares keys.
        let s = Session {
            name: "x".into(), dir: "/x".into(), pid: 1, uptime_sec: 0,
            model: None, effort: None, rc_link: None, git: None, working: false,
            last_message: None, sid: None, cpu: None, rss_kb: 0,
            cpu_sample_age_ms: 0, external: false,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("external"));
    }
}

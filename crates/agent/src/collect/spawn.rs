use super::ctx::{Ctx, Meta};
use super::fsio::{read_if, write_atomic};
use super::lookup::rc_link_for;
use super::validate::{
    assert_effort, assert_kill_name, assert_model, assert_valid_sid, BadRequest,
};
use crate::parse::history::group_history;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 60 × 500 ms = the 30 s budget from `lib/collect.js:143`.
pub const RC_POLL_ATTEMPTS: u32 = 60;
pub const RC_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Mark a directory trusted in `~/.claude.json` so `claude` does not open on a
/// trust prompt no one is sitting in front of. Read-modify-write: every other
/// key in the file is preserved, and the write is atomic.
pub async fn trust_dir(claude_json: &Path, dir: &str) -> std::io::Result<()> {
    let mut cfg: serde_json::Value = read_if(claude_json)
        .await
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !cfg.is_object() {
        cfg = serde_json::json!({});
    }
    let projects = cfg
        .as_object_mut()
        .expect("cfg is an object")
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    if !projects.is_object() {
        *projects = serde_json::json!({});
    }
    let entry = projects
        .as_object_mut()
        .expect("projects is an object")
        .entry(dir)
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    entry
        .as_object_mut()
        .expect("entry is an object")
        .insert("hasTrustDialogAccepted".into(), serde_json::Value::Bool(true));

    let json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".into());
    write_atomic(claude_json, &json, ".cdash.tmp").await
}

/// `~/.claude.json` lives in `$HOME` even when `CLAUDE_DIR` is overridden, but
/// for testability it is derived from `claude_dir`'s parent when the override
/// is set — the same rule as `lib/collect.js:114-116`.
pub fn claude_json_path(claude_dir: &Path) -> PathBuf {
    if std::env::var("CLAUDE_DIR").is_ok() {
        claude_dir.parent().unwrap_or(Path::new("/")).join(".claude.json")
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())).join(".claude.json")
    }
}

/// `cdash-<base>-<HHMM>-<xxx>`. The shape is load-bearing: it is what the pane
/// filter matches and what the kill guard admits.
pub fn tmux_name(dir: &str) -> String {
    let base: String = Path::new(dir)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .take(30)
        .collect();

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs_of_day = now.as_secs() % 86_400;
    let hhmm = format!("{:02}{:02}", secs_of_day / 3600, (secs_of_day % 3600) / 60);

    // ponytail: nanosecond entropy stands in for Math.random()'s 3 base36
    // chars and avoids a `rand` dependency. Collisions need the same directory,
    // the same minute, and the same nanosecond bucket.
    let n = now.subsec_nanos();
    let suffix: String = (0..3)
        .map(|i| char::from_digit((n >> (i * 6)) % 36, 36).unwrap_or('0'))
        .collect();

    format!("cdash-{base}-{hhmm}-{suffix}")
}

/// Wait for `claude` to publish its remote-control session id, then record the
/// link. Two guards, both required: the session can be killed while this
/// sleeps, and again between the read and the write.
pub async fn poll_rc_link(
    ctx: Arc<Ctx>,
    name: String,
    pid: i32,
    attempts: u32,
    interval: Duration,
) {
    for i in 0..attempts {
        tokio::time::sleep(interval).await;
        if !ctx.meta_has(&name) {
            return; // killed while polling
        }
        if let Some(link) = rc_link_for(&ctx.claude_dir, pid).await {
            if !ctx.meta_update(&name, |m| m.rc_link = Some(link.clone())) {
                return; // killed between the check and the write
            }
            ctx.host.log.push(format!(
                "rc-link captured {name} ({}s)",
                (i + 1) as f64 * interval.as_secs_f64()
            ));
            return;
        }
    }
    ctx.host.log.push(format!(
        "rc-link timeout {name} ({}s)",
        attempts as f64 * interval.as_secs_f64()
    ));
}

async fn spawn_claude(
    ctx: &Arc<Ctx>,
    dir: &str,
    claude_args: &[&str],
    meta: Meta,
) -> Result<String, BadRequest> {
    if let Err(e) = trust_dir(&claude_json_path(&ctx.claude_dir), dir).await {
        ctx.host.log.push(format!("trust write failed for {dir}: {e}"));
    }
    let name = tmux_name(dir);

    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &name, "-c", dir, "claude"];
    args.extend_from_slice(claude_args);
    args.extend_from_slice(&["--dangerously-skip-permissions", "--remote-control", &name]);
    ctx.runner.run("tmux", &args, "tmux new-session").await;

    ctx.meta_set(&name, meta);
    ctx.host.log.push(format!(
        "launch {} → {name}",
        Path::new(dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));

    let pid_out = ctx
        .runner
        .run(
            "tmux",
            &["display-message", "-p", "-t", &name, "#{pane_pid}"],
            "tmux display-message",
        )
        .await;
    if let Ok(pid) = pid_out.trim().parse::<i32>() {
        tokio::spawn(poll_rc_link(
            Arc::clone(ctx),
            name.clone(),
            pid,
            RC_POLL_ATTEMPTS,
            RC_POLL_INTERVAL,
        ));
    } else {
        ctx.host.log.push(format!("rc-poll skipped {name}: no pane pid"));
    }

    Ok(name)
}

pub async fn launch_session(
    ctx: &Arc<Ctx>,
    dir: &str,
    model: &str,
    effort: &str,
) -> Result<String, BadRequest> {
    assert_model(model)?;
    assert_effort(effort)?;
    // A6: the directory must exist and be a directory before it reaches `-c`.
    let is_dir = tokio::fs::metadata(dir).await.map(|m| m.is_dir()).unwrap_or(false);
    if !is_dir {
        return Err(BadRequest(format!("not a directory: {dir}")));
    }
    spawn_claude(
        ctx,
        dir,
        &["--model", model, "--effort", effort],
        Meta { model: Some(model.into()), effort: Some(effort.into()), rc_link: None },
    )
    .await
}

pub async fn resume_session(ctx: &Arc<Ctx>, sid: &str) -> Result<String, BadRequest> {
    assert_valid_sid(sid)?;
    let hist = read_if(&ctx.claude_dir.join("history.jsonl")).await.unwrap_or_default();
    let cwd = group_history(&hist)
        .into_iter()
        .find(|g| g.sid == sid)
        .and_then(|g| g.cwd)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| BadRequest(format!("unknown session: {sid}")))?;

    // D6: un-purge before spawning, so the resumed session is not filtered
    // straight back out of the resumable list it came from.
    ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);

    spawn_claude(ctx, &cwd, &["--resume", sid], Meta::default()).await
}

/// Kill a session this dashboard owns. The name is guarded first — it becomes
/// a `tmux` argument — and the meta entry is dropped whether or not tmux
/// agreed, which is what stops a poll still in flight from resurrecting it.
pub async fn kill_session(ctx: &Arc<Ctx>, name: &str) -> Result<(), BadRequest> {
    assert_kill_name(name)?;
    ctx.runner.run("tmux", &["kill-session", "-t", name], "tmux kill-session").await;
    ctx.meta_delete(name);
    ctx.host.log.push(format!("kill {name}"));
    Ok(())
}

/// Hide a resumable session from the list. Purely a note to ourselves — no
/// file is touched and the transcript is not deleted.
pub fn purge_session(ctx: &Arc<Ctx>, sid: &str) -> Result<(), BadRequest> {
    assert_valid_sid(sid)?;
    ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).insert(sid.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::cmd::Runner;
    use crate::host::log::LogBuffer;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-spawn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    async fn ctx_for(claude_dir: PathBuf) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let host = crate::host::init::Host {
            runner: Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, claude_dir, None))
    }

    #[test]
    fn tmux_name_is_prefixed_munged_and_length_capped() {
        let n = tmux_name("/mnt/d/git/a project.with-dots");
        assert!(n.starts_with("cdash-"));
        assert!(!n.contains(' ') && !n.contains('.'));
        // B7: the base is capped at 30 chars, before the time and suffix.
        let base: &str = n.split('-').nth(1).unwrap();
        assert!(base.len() <= 30);
        // The result must survive the kill-target guard it will later be given to.
        crate::collect::validate::assert_kill_name(&n).unwrap();
    }

    #[test]
    fn two_names_for_the_same_directory_differ() {
        assert_ne!(tmux_name("/x/y"), tmux_name("/x/y"));
    }

    #[test]
    fn the_poll_budget_is_thirty_seconds() {
        // C4: the attempt count and interval are separately adjustable, so the
        // product is what actually needs pinning.
        assert_eq!(RC_POLL_INTERVAL * RC_POLL_ATTEMPTS, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn the_write_step_alone_refuses_a_session_that_is_already_gone() {
        // D4 in isolation. The pair test below cannot prove this guard on its
        // own — the first guard returns before the second is ever reached — so
        // the write step is exercised directly.
        let d = tempdir("commit-gone");
        let ctx = ctx_for(d).await;
        assert!(!ctx.meta_update("cdash-vanished", |m| m.rc_link = Some("https://claude.ai/code/x".into())));
        assert!(ctx.meta_get("cdash-vanished").is_none());

        ctx.meta_set("cdash-here", Meta::default());
        assert!(ctx.meta_update("cdash-here", |m| m.rc_link = Some("https://claude.ai/code/y".into())));
        assert_eq!(
            ctx.meta_get("cdash-here").unwrap().rc_link.as_deref(),
            Some("https://claude.ai/code/y")
        );
    }

    #[tokio::test]
    async fn trust_dir_marks_the_directory_and_preserves_every_other_key() {
        // D8: this file is the user's real ~/.claude.json. Losing an unrelated
        // key here is data loss, not a cosmetic bug.
        let d = tempdir("trust");
        let f = d.join(".claude.json");
        tokio::fs::write(
            &f,
            r#"{"theme":"dark","projects":{"/other":{"hasTrustDialogAccepted":true,"note":"keep me"}}}"#,
        )
        .await
        .unwrap();

        trust_dir(&f, "/new").await.unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&f).await.unwrap()).unwrap();
        assert_eq!(v["theme"], "dark", "unrelated top-level keys survive");
        assert_eq!(v["projects"]["/other"]["note"], "keep me", "other projects survive");
        assert_eq!(v["projects"]["/new"]["hasTrustDialogAccepted"], true);
        assert!(!d.join(".claude.json.cdash.tmp").exists(), "D1: temp file renamed away");
    }

    #[tokio::test]
    async fn trust_dir_creates_the_file_when_it_does_not_exist_yet() {
        let d = tempdir("trust-fresh");
        let f = d.join(".claude.json");
        trust_dir(&f, "/new").await.unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&f).await.unwrap()).unwrap();
        assert_eq!(v["projects"]["/new"]["hasTrustDialogAccepted"], true);
    }

    #[tokio::test]
    async fn the_rc_poll_writes_the_link_it_finds() {
        let d = tempdir("rc-ok");
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-x", Meta::default());
        tokio::fs::write(
            d.join("sessions/99.json"),
            r#"{"bridgeSessionId":"session_found"}"#,
        )
        .await
        .unwrap();

        poll_rc_link(Arc::clone(&ctx), "cdash-x".into(), 99, 3, Duration::from_millis(10)).await;

        assert_eq!(
            ctx.meta_get("cdash-x").unwrap().rc_link.as_deref(),
            Some("https://claude.ai/code/session_found")
        );
    }

    #[tokio::test]
    async fn a_session_killed_during_the_poll_is_not_resurrected() {
        // D4: the guard with no test in Node. Without it, a poll that started
        // before the kill writes the entry back after `meta.delete`, and a dead
        // session reappears in the UI holding a live-looking RC link.
        let d = tempdir("rc-killed");
        let ctx = ctx_for(d.clone()).await;
        tokio::fs::write(
            d.join("sessions/98.json"),
            r#"{"bridgeSessionId":"session_ghost"}"#,
        )
        .await
        .unwrap();
        // meta deliberately NOT set — this stands for "killed before the tick".

        poll_rc_link(Arc::clone(&ctx), "cdash-dead".into(), 98, 3, Duration::from_millis(10)).await;

        assert!(
            ctx.meta_get("cdash-dead").is_none(),
            "the poll must not create an entry for a session that is gone"
        );
    }

    #[tokio::test]
    async fn the_poll_gives_up_after_its_attempt_budget() {
        // B6/C4: bounded, so a session that never publishes a link does not
        // leave a task polling forever.
        let d = tempdir("rc-timeout");
        let ctx = ctx_for(d).await;
        ctx.meta_set("cdash-y", Meta::default());
        let started = std::time::Instant::now();

        poll_rc_link(Arc::clone(&ctx), "cdash-y".into(), 1, 3, Duration::from_millis(10)).await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(ctx.meta_get("cdash-y").unwrap().rc_link, None);
        assert!(ctx.host.log.lines().iter().any(|l| l.contains("rc-link timeout")));
    }

    #[tokio::test]
    async fn launch_rejects_a_bad_model_effort_or_directory_before_spawning() {
        let d = tempdir("launch-guard");
        let ctx = ctx_for(d.clone()).await;
        assert!(launch_session(&ctx, "/tmp", "gpt-4", "medium").await.is_err());
        assert!(launch_session(&ctx, "/tmp", "sonnet", "ludicrous").await.is_err());
        // A6: a path that is not a directory never reaches `tmux -c`.
        assert!(launch_session(&ctx, "/no/such/dir/cdash", "sonnet", "medium").await.is_err());
    }

    #[tokio::test]
    async fn resume_rejects_a_bad_sid_before_touching_the_shell() {
        let d = tempdir("resume-guard");
        let ctx = ctx_for(d).await;
        assert!(resume_session(&ctx, "not-a-uuid; rm -rf /").await.is_err());
        assert!(resume_session(&ctx, "").await.is_err());
    }

    #[tokio::test]
    async fn resume_rejects_a_sid_that_history_does_not_know() {
        let d = tempdir("resume-unknown");
        let ctx = ctx_for(d.clone()).await;
        tokio::fs::write(d.join("history.jsonl"), "").await.unwrap();
        let e = resume_session(&ctx, "2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34").await.unwrap_err();
        assert!(e.0.contains("unknown session"));
    }

    #[tokio::test]
    async fn resume_un_purges_the_session_it_is_bringing_back() {
        // D6: without this the resumed session is filtered straight back out
        // of the list it was resumed from, and the row never reappears.
        // PATH is pointed at nothing so the tmux call is a no-op — this test
        // must not start a real session.
        let d = tempdir("resume-unpurge");
        let log = Arc::new(LogBuffer::new());
        let host = crate::host::init::Host {
            runner: Runner::new("/nonexistent-for-test".into(), Arc::clone(&log)),
            log,
            path: "/nonexistent-for-test".into(),
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        let ctx = Arc::new(Ctx::new(host, d.clone(), None));

        let sid = "2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34";
        tokio::fs::write(
            d.join("history.jsonl"),
            format!("{{\"sessionId\":\"{sid}\",\"project\":\"/tmp\",\"timestamp\":1,\"display\":\"x\"}}\n"),
        )
        .await
        .unwrap();
        purge_session(&ctx, sid).unwrap();
        assert!(ctx.purged.lock().unwrap().contains(sid));

        resume_session(&ctx, sid).await.unwrap();
        assert!(
            !ctx.purged.lock().unwrap().contains(sid),
            "a resumed session must stop being hidden"
        );
    }

    #[tokio::test]
    async fn kill_rejects_a_name_that_is_not_ours_before_running_tmux() {
        // A2: the argument reaches `tmux kill-session -t`.
        let d = tempdir("kill-guard");
        let ctx = ctx_for(d).await;
        assert!(kill_session(&ctx, "other").await.is_err());
        assert!(kill_session(&ctx, "cdash-x; rm -rf /").await.is_err());
    }

    #[tokio::test]
    async fn kill_forgets_the_session_meta() {
        // D7: a stale meta entry would let the RC poll write a link back for a
        // session that no longer exists.
        let d = tempdir("kill-meta");
        let ctx = ctx_for(d).await;
        ctx.meta_set("cdash-gone-1200-abc", Meta::default());
        // tmux is not running a session by this name; the kill fails and the
        // meta must be dropped anyway, exactly as Node's route did.
        let _ = kill_session(&ctx, "cdash-gone-1200-abc").await;
        assert!(!ctx.meta_has("cdash-gone-1200-abc"));
    }

    #[tokio::test]
    async fn purge_rejects_a_bad_sid_and_records_a_good_one() {
        let d = tempdir("purge");
        let ctx = ctx_for(d).await;
        assert!(purge_session(&ctx, "../../etc/passwd").is_err());
        purge_session(&ctx, "2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34").unwrap();
        assert!(ctx
            .purged
            .lock()
            .unwrap()
            .contains("2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34"));
    }
}

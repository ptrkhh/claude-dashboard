use super::ctx::{Ctx, Meta};
#[cfg(windows)]
use super::external::live_session_files;
use super::fsio::{read_if, write_atomic};
use super::lookup::{rc_link_for, rc_link_from};
use super::side::{side_for, Backend, Side};
use super::validate::{
    assert_effort, assert_kill_name, assert_model, assert_valid_sid, BadRequest, Refused,
};
use crate::parse::history::group_history;
#[cfg(windows)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 60 × 500 ms = the 30 s budget from `lib/collect.js:143`.
pub const RC_POLL_ATTEMPTS: u32 = 60;
pub const RC_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The explicit Remote Control request must win over this user-level opt-out,
/// but only for the dashboard child; the user's settings file stays untouched.
pub const SETTINGS_JSON: &str = r#"{"env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}"#;

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

/// `~/.claude.json` sits beside the Claude directory on every side: the
/// native `~/.claude` and a WSL side's `\\wsl.localhost\…\home\u\.claude`
/// alike. This resolves to the same file the old `HOME`-or-`CLAUDE_DIR` rule
/// did in both of its cases, and reads nothing from the environment.
pub fn claude_json_path(claude_dir: &Path) -> PathBuf {
    claude_dir.parent().unwrap_or(Path::new("/")).join(".claude.json")
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

/// How the RC-link poll finds the session file: by the pane pid tmux reported,
/// or, where nothing reported a pid, by the `--name` the launcher passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RcLocator {
    ByPid(i32),
    ByName,
}

/// The RC link of the session file whose `name` is `name`, if it has one yet.
pub async fn rc_link_by_name(claude_dir: &Path, name: &str) -> Option<String> {
    let mut entries = tokio::fs::read_dir(claude_dir.join("sessions")).await.ok()?;
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(txt) = read_if(&p).await else { continue };
        // Read as a `Value`, not as `SessionFile`: one wrongly-typed field
        // anywhere in the file — a numeric `bridgeSessionId`, a string
        // `startedAt` — fails the typed parse and would drop the name match
        // with it, timing out a session the by-pid arm would have linked.
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
        if v.get("name").and_then(|n| n.as_str()) == Some(name) {
            if let Some(link) = rc_link_from(&txt) {
                return Some(link);
            }
        }
    }
    None
}

/// Wait for `claude` to publish its remote-control session id, then record the
/// link. Two guards, both required: the session can be killed while this
/// sleeps, and again between the read and the write.
pub async fn poll_rc_link(
    ctx: Arc<Ctx>,
    claude_dir: PathBuf,
    name: String,
    locator: RcLocator,
    attempts: u32,
    interval: Duration,
) {
    for i in 0..attempts {
        tokio::time::sleep(interval).await;
        if !ctx.meta_has(&name) {
            return; // killed while polling
        }
        let found = match locator {
            RcLocator::ByPid(pid) => rc_link_for(&claude_dir, pid).await,
            RcLocator::ByName => rc_link_by_name(&claude_dir, &name).await,
        };
        if let Some(link) = found {
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

/// Start `claude` on one side. The argv is the same everywhere; the backend
/// decides what wraps it: a detached tmux session, or its own console window.
/// `--name` makes the session file carry the same name on every backend.
async fn spawn_claude(
    ctx: &Arc<Ctx>,
    side: &Side,
    dir: &str,
    claude_args: &[&str],
    meta: Meta,
) -> Result<String, Refused> {
    if let Err(e) = trust_dir(&claude_json_path(&side.claude_dir), dir).await {
        ctx.host.log.push(format!("trust write failed for {dir}: {e}"));
    }
    let name = tmux_name(dir);

    let mut argv: Vec<&str> = vec!["claude", "--settings", SETTINGS_JSON];
    argv.extend_from_slice(claude_args);
    argv.extend_from_slice(&[
        "--dangerously-skip-permissions",
        "--remote-control",
        &name,
        "--name",
        &name,
    ]);

    match side.backend {
        Backend::Tmux => {
            let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &name, "-c", dir];
            args.extend_from_slice(&argv);
            // Checked: without this a failed `tmux new-session` still answered
            // 200 with a session name, and left a meta entry for a session
            // that does not exist.
            side.runner
                .run_checked("tmux", &args, "tmux new-session")
                .await
                .map_err(Refused::Failed)?;
        }
        #[cfg(windows)]
        Backend::Console => {
            side.runner
                .spawn_detached(argv[0], &argv[1..], dir, "claude spawn")
                .map_err(Refused::Failed)?;
        }
    }

    ctx.meta_set(&name, meta);
    ctx.host.log.push(format!(
        "launch {} → {name}",
        Path::new(dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));

    // The pid lookup stays best-effort — losing it costs the rc-link poll,
    // not the session. A console launch never has one and polls by name.
    let locator = match side.backend {
        Backend::Tmux => {
            let pid_out = side
                .runner
                .run(
                    "tmux",
                    &["display-message", "-p", "-t", &name, "#{pane_pid}"],
                    "tmux display-message",
                )
                .await;
            match pid_out.trim().parse::<i32>() {
                Ok(pid) => Some(RcLocator::ByPid(pid)),
                Err(_) => {
                    ctx.host.log.push(format!("rc-poll skipped {name}: no pane pid"));
                    None
                }
            }
        }
        #[cfg(windows)]
        Backend::Console => Some(RcLocator::ByName),
    };
    if let Some(locator) = locator {
        tokio::spawn(poll_rc_link(
            Arc::clone(ctx),
            side.claude_dir.clone(),
            name.clone(),
            locator,
            RC_POLL_ATTEMPTS,
            RC_POLL_INTERVAL,
        ));
    }

    Ok(name)
}

/// What a launch reports back: the session name, and whether it landed on the
/// native side — the recents entry is resolved only for native paths.
#[derive(Debug)]
pub struct Launched {
    pub name: String,
    pub native: bool,
}

pub async fn launch_session(
    ctx: &Arc<Ctx>,
    dir: &str,
    model: &str,
    effort: &str,
) -> Result<Launched, Refused> {
    assert_model(model)?;
    assert_effort(effort)?;
    let (side, dir) = side_for(&ctx.sides, dir)?;
    // A6: the directory must exist and be a directory before it reaches `-c`.
    // A WSL directory is checked through the share.
    let check = match &side.wsl {
        Some(w) => PathBuf::from(w.to_unc(&dir)),
        None => PathBuf::from(&dir),
    };
    let is_dir = tokio::fs::metadata(&check).await.map(|m| m.is_dir()).unwrap_or(false);
    if !is_dir {
        return Err(Refused::BadRequest(format!("not a directory: {dir}")));
    }
    let name = spawn_claude(
        ctx,
        side,
        &dir,
        &["--model", model, "--effort", effort],
        Meta { model: Some(model.into()), effort: Some(effort.into()), rc_link: None },
    )
    .await?;
    Ok(Launched { name, native: !side.is_wsl() })
}

pub async fn resume_session(ctx: &Arc<Ctx>, sid: &str) -> Result<String, Refused> {
    assert_valid_sid(sid)?;
    // The side is whichever history holds the sid; its cwd is already in
    // that side's notation.
    let mut found: Option<(&Side, String)> = None;
    for side in &ctx.sides {
        let hist = read_if(&side.claude_dir.join("history.jsonl")).await.unwrap_or_default();
        let cwd = group_history(&hist)
            .into_iter()
            .find(|g| g.sid == sid)
            .and_then(|g| g.cwd)
            .filter(|c| !c.is_empty());
        if let Some(cwd) = cwd {
            found = Some((side, cwd));
            break;
        }
    }
    let (side, cwd) =
        found.ok_or_else(|| Refused::BadRequest(format!("unknown session: {sid}")))?;

    // D6: un-purge before spawning, so the resumed session is not filtered
    // straight back out of the resumable list it came from.
    ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);

    spawn_claude(ctx, side, &cwd, &["--resume", sid], Meta::default()).await
}

/// Kill a session this dashboard owns. The name is guarded first — it becomes
/// a `tmux` or `taskkill` argument. Console sides are searched first, with the
/// same liveness scan the list uses, so a stale file whose pid was recycled
/// never matches and nothing foreign is killed; then every tmux side is tried.
pub async fn kill_session(ctx: &Arc<Ctx>, name: &str) -> Result<(), Refused> {
    assert_kill_name(name)?;

    #[cfg(windows)]
    for side in ctx.sides.iter().filter(|s| matches!(s.backend, Backend::Console)) {
        let rows = side.proc_rows(&ctx.host.sampler).await;
        let hit = live_session_files(side, &rows, &HashSet::new())
            .await
            .into_iter()
            .find(|(_, s)| s.name.as_deref() == Some(name));
        if let Some((pid, _)) = hit {
            let pid = pid.to_string();
            side.runner
                .run_checked("taskkill", &["/T", "/F", "/PID", &pid], "taskkill")
                .await
                .map_err(Refused::Failed)?;
            ctx.meta_delete(name);
            ctx.host.log.push(format!("kill {name} (pid {pid})"));
            return Ok(());
        }
    }

    // Checked, and the meta entry is dropped only after the kill succeeds:
    // reporting a kill that did not happen removes the card from the UI while
    // the session keeps running. The first failure is the reported one, so the
    // message does not depend on how many sides were tried after it.
    let mut first_err = None;
    for side in ctx.sides.iter().filter(|s| matches!(s.backend, Backend::Tmux)) {
        match side
            .runner
            .run_checked("tmux", &["kill-session", "-t", name], "tmux kill-session")
            .await
        {
            Ok(_) => {
                ctx.meta_delete(name);
                ctx.host.log.push(format!("kill {name}"));
                return Ok(());
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    Err(Refused::Failed(first_err.unwrap_or_else(|| format!("no such session: {name}"))))
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

    #[test]
    fn claude_json_lives_beside_the_claude_dir_on_every_side() {
        // The WSL side's file is `\\wsl.localhost\…\home\u\.claude.json`,
        // beside its `.claude`; deriving from the directory rather than from
        // HOME is what makes one rule serve both sides.
        assert_eq!(
            claude_json_path(Path::new("/home/u/.claude")),
            PathBuf::from("/home/u/.claude.json")
        );
        assert_eq!(
            claude_json_path(Path::new("/custom/dir")),
            PathBuf::from("/custom/.claude.json")
        );
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

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-x".into(), RcLocator::ByPid(99), 3, Duration::from_millis(10)).await;

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

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-dead".into(), RcLocator::ByPid(98), 3, Duration::from_millis(10)).await;

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
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-y", Meta::default());
        let started = std::time::Instant::now();

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-y".into(), RcLocator::ByPid(1), 3, Duration::from_millis(10)).await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(ctx.meta_get("cdash-y").unwrap().rc_link, None);
        assert!(ctx.host.log.lines().iter().any(|l| l.contains("rc-link timeout")));
    }

    #[tokio::test]
    async fn the_rc_poll_can_find_the_session_by_name() {
        // A console launch knows no pid: the session file is found by the
        // `--name` the launcher passed, not by a pid we never learned.
        let d = tempdir("rc-name");
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-byname", Meta::default());
        tokio::fs::write(
            d.join("sessions/77.json"),
            r#"{"name":"cdash-byname","bridgeSessionId":"session_named"}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            d.join("sessions/78.json"),
            r#"{"name":"other","bridgeSessionId":"session_other"}"#,
        )
        .await
        .unwrap();

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-byname".into(), RcLocator::ByName, 3, Duration::from_millis(10)).await;

        assert_eq!(
            ctx.meta_get("cdash-byname").unwrap().rc_link.as_deref(),
            Some("https://claude.ai/code/session_named")
        );
    }

    #[tokio::test]
    async fn the_by_name_poll_stringifies_a_numeric_bridge_id() {
        // Both locators must agree on what a bridge id is: `parse_rc_file`
        // stringifies a numeric one rather than dropping the link with it.
        let d = tempdir("rc-name-numeric");
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-num", Meta::default());
        tokio::fs::write(
            d.join("sessions/79.json"),
            r#"{"name":"cdash-num","bridgeSessionId":12345}"#,
        )
        .await
        .unwrap();

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-num".into(), RcLocator::ByName, 3, Duration::from_millis(10)).await;

        assert_eq!(
            ctx.meta_get("cdash-num").unwrap().rc_link.as_deref(),
            Some("https://claude.ai/code/12345")
        );
    }

    #[tokio::test]
    async fn a_named_session_without_a_bridge_id_yet_stays_retryable() {
        // The session file appears before the id does. The poll must keep
        // looking rather than record a link it has not been given.
        let d = tempdir("rc-name-pending");
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-pending", Meta::default());
        tokio::fs::write(d.join("sessions/80.json"), r#"{"name":"cdash-pending"}"#).await.unwrap();

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-pending".into(), RcLocator::ByName, 3, Duration::from_millis(10)).await;

        assert_eq!(ctx.meta_get("cdash-pending").unwrap().rc_link, None);
        assert!(
            ctx.host.log.lines().iter().any(|l| l.contains("rc-link timeout")),
            "a file without an id yet must be retried, not treated as an answer"
        );
    }

    #[tokio::test]
    async fn the_by_name_poll_gives_up_after_its_attempt_budget() {
        // B6/C4 for the ByName arm, as `the_poll_gives_up_after_its_attempt_budget`
        // pins it for ByPid: a name that never appears is bounded too.
        let d = tempdir("rc-name-timeout");
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-z", Meta::default());
        tokio::fs::write(d.join("sessions/81.json"), r#"{"name":"someone-else"}"#).await.unwrap();
        let started = std::time::Instant::now();

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-z".into(), RcLocator::ByName, 3, Duration::from_millis(10)).await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(ctx.meta_get("cdash-z").unwrap().rc_link, None);
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
        // An unknown sid is the caller's input, not our subprocess: 400, not 500.
        assert!(matches!(&e, Refused::BadRequest(m) if m.contains("unknown session")), "got {e:?}");
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

    /// Everything that needs the stub `tmux`. Unix-only: the stub is chmodded
    /// executable and written with a `#!/bin/sh` shebang, neither of which
    /// means anything on Windows, where the native side is a console anyway.
    #[cfg(unix)]
    mod tmux_tests {
        use super::*;

        /// A stub `tmux` whose exit status the test chooses, so both sides of
        /// the checked-subprocess branch are reachable without a real tmux
        /// server.
        fn stub_tmux(dir: &Path, exit: i32) -> String {
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let p = bin.join("tmux");
            std::fs::write(
                &p,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/tmux-args'\necho 4242\nexit {exit}\n",
                    dir.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin.to_string_lossy().into_owned()
        }

        async fn ctx_with_tmux(claude_dir: PathBuf, exit: i32) -> Arc<Ctx> {
            let path = stub_tmux(&claude_dir, exit);
            let log = Arc::new(LogBuffer::new());
            let host = crate::host::init::Host {
                runner: Runner::new(path.clone(), Arc::clone(&log)),
                log,
                path,
                sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
            };
            Arc::new(Ctx::new(host, claude_dir, None))
        }

        #[tokio::test]
        async fn resume_un_purges_the_session_it_is_bringing_back() {
            // D6: without this the resumed session is filtered straight back out
            // of the list it was resumed from, and the row never reappears.
            // A stub tmux that exits 0, so this test never starts a real session.
            // It cannot point PATH at nothing any more: resume is checked now, and
            // a tmux that fails to spawn is a failed resume rather than a no-op.
            let d = tempdir("resume-unpurge");
            let ctx = ctx_with_tmux(d.clone(), 0).await;

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
        async fn kill_forgets_the_session_meta() {
            // D7: a stale meta entry would let the RC poll write a link back for a
            // session that no longer exists.
            let ctx = ctx_with_tmux(tempdir("kill-meta"), 0).await;
            ctx.meta_set("cdash-gone-1200-abc", Meta::default());
            kill_session(&ctx, "cdash-gone-1200-abc").await.unwrap();
            assert!(!ctx.meta_has("cdash-gone-1200-abc"));
        }

        #[tokio::test]
        async fn a_kill_that_failed_is_reported_and_keeps_the_session() {
            // The regression this exists for: `Runner::run` returns an empty string
            // on a non-zero exit, so the route answered 200 {"ok":true} while the
            // session kept running and the card vanished from the UI. Node's route
            // used the throwing `run` and returned 500.
            let ctx = ctx_with_tmux(tempdir("kill-fails"), 1).await;
            ctx.meta_set("cdash-alive-1200-abc", Meta::default());

            let e = kill_session(&ctx, "cdash-alive-1200-abc").await.unwrap_err();
            assert!(matches!(&e, Refused::Failed(m) if m.contains("kill-session")), "got {e:?}");
            assert!(
                ctx.meta_has("cdash-alive-1200-abc"),
                "a session we failed to kill must not be forgotten"
            );
        }

        #[tokio::test]
        async fn a_launch_whose_tmux_failed_leaves_no_phantom_session() {
            let d = tempdir("launch-fails");
            let ctx = ctx_with_tmux(d.clone(), 1).await;
            let e = launch_session(&ctx, d.to_str().unwrap(), "sonnet", "medium").await.unwrap_err();
            assert!(matches!(&e, Refused::Failed(m) if m.contains("new-session")), "got {e:?}");
            assert!(
                ctx.meta.lock().unwrap().is_empty(),
                "a session that was never created must leave no meta entry"
            );
        }

        #[tokio::test]
        async fn launch_overrides_the_setting_that_disables_remote_control() {
            let d = tempdir("rc-setting");
            let ctx = ctx_with_tmux(d.clone(), 0).await;

            launch_session(&ctx, d.to_str().unwrap(), "sonnet", "medium").await.unwrap();

            let args = std::fs::read_to_string(d.join("tmux-args")).unwrap();
            assert!(
                args.lines().next().unwrap().contains(
                    r#"--settings {"env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}"#
                ),
                "dashboard launches must keep their explicitly requested Remote Control enabled: {args}"
            );
        }

        #[tokio::test]
        async fn resume_finds_the_sid_in_a_later_sides_history_and_names_the_session() {
            // Two native sides stand in for native + WSL: the sid lives only
            // in the second side's history, so that is where the spawn goes.
            let d1 = tempdir("resume-side-1");
            let d2 = tempdir("resume-side-2");
            let path = stub_tmux(&d1, 0);
            let log = Arc::new(LogBuffer::new());
            let host = crate::host::init::Host {
                runner: Runner::new(path.clone(), Arc::clone(&log)),
                log,
                path,
                sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
            };
            let mut c = Ctx::new(host, d1.clone(), None);
            c.sides.push(crate::collect::side::Side::native(d2.clone(), Arc::clone(&c.runner)));
            let ctx = Arc::new(c);

            let sid = "3f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34";
            tokio::fs::write(d1.join("history.jsonl"), "").await.unwrap();
            tokio::fs::write(
                d2.join("history.jsonl"),
                format!("{{\"sessionId\":\"{sid}\",\"project\":\"/tmp\",\"timestamp\":1,\"display\":\"x\"}}\n"),
            )
            .await
            .unwrap();

            let name = resume_session(&ctx, sid).await.unwrap();

            let args = std::fs::read_to_string(d1.join("tmux-args")).unwrap();
            assert!(args.contains(&format!("--resume {sid}")), "{args}");
            assert!(args.contains(&format!("--name {name}")), "the session file will carry our name: {args}");
            assert!(args.contains("-c /tmp"), "{args}");
        }
    }
}

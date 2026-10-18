//! Managed Codex sessions share the tmux lifecycle; their bridge owns metadata.
use super::ctx::Ctx;
use super::fsio::{read_if, write_atomic};
use super::sessions::Resumable;
use super::spawn::tmux_name;
use super::validate::{assert_effort, assert_valid_sid, is_codex_model, Refused};
use crate::host::probe::has_binary;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChoice {
    pub value: String,
    pub model: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
}

pub async fn models(ctx: &Ctx) -> Result<Vec<ModelChoice>, Refused> {
    // ponytail: one catalog per agent for 60s; key by project if catalogs become project-specific.
    let mut cache = ctx.codex_models.lock().await;
    if let Some((when, choices)) = cache.as_ref() {
        if when.elapsed() < Duration::from_secs(60) {
            return Ok(choices.clone());
        }
    }
    let choices = discover_models(ctx).await?;
    *cache = Some((Instant::now(), choices.clone()));
    Ok(choices)
}

async fn discover_models(ctx: &Ctx) -> Result<Vec<ModelChoice>, Refused> {
    serde_json::from_value(query(ctx, "--models").await.map_err(Refused::Failed)?)
        .map_err(|e| Refused::Failed(format!("Codex model catalog: {e}")))
}

pub(super) async fn query(ctx: &Ctx, mode: &str) -> Result<serde_json::Value, String> {
    // Codex and Node are optional: a Claude-only installation is healthy.
    if !has_binary(&ctx.host.path, "codex") || !has_binary(&ctx.host.path, "node") {
        return Ok(serde_json::json!([]));
    }
    let json = ctx
        .runner
        .run_checked_with_timeout(
            "node",
            &[
                "--input-type=module",
                "-e",
                include_str!("codex-remote.mjs"),
                "--",
                mode,
            ],
            &format!("Codex metadata {mode}"),
            Duration::from_secs(15),
        )
        .await?;
    let value: serde_json::Value = serde_json::from_str(&json)
        .map_err(|_| "Codex metadata returned invalid JSON".to_string())?;
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        return Err(error.to_owned());
    }
    Ok(value)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexSession {
    #[serde(default)]
    pub native_terminal: bool,
    #[serde(default)]
    pub rc_unavailable: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    pub dir: String,
    pub pid: Option<i32>,
    pub sid: Option<String>,
    #[serde(default)]
    pub working: bool,
    pub last_message: Option<String>,
    #[serde(default)]
    pub ts: f64,
    pub effort: Option<String>,
    pub model: Option<String>,
    pub model_label: Option<String>,
}

fn root(ctx: &Ctx) -> PathBuf {
    ctx.claude_dir.join("cdash-codex")
}

/// A saved legacy bridge is not an interactive terminal. Missing state means
/// this is an ordinary Claude/tmux session; corrupt Codex state fails closed.
pub async fn terminal_supported(ctx: &Ctx, name: &str) -> bool {
    let Some(text) = read_if(&root(ctx).join(format!("{name}.json"))).await else { return true };
    let Ok(state) = serde_json::from_str::<CodexSession>(&text) else { return false };
    if state.native_terminal { return true; }
    let Some(pid) = state.pid else { return false };
    let target = format!("={name}:");
    ctx.runner.run_checked("tmux", &["display-message", "-p", "-t", &target, "#{pane_pid}"], "tmux terminal pane")
        .await.ok().and_then(|s| s.trim().parse::<i32>().ok()).is_some_and(|actual| actual != pid)
}

pub async fn snapshot(ctx: &Ctx, name: &str, pid: i32) -> Option<CodexSession> {
    let s: CodexSession =
        serde_json::from_str(&read_if(&root(ctx).join(format!("{name}.json"))).await?).ok()?;
    // An archived snapshot must not claim a later pane that reused its name.
    if s.pid != Some(pid) { return None; }
    // Native metadata belongs to the supervisor, so an RC outage cannot freeze
    // it. The worker separately owns its remote delivery checkpoint.
    if s.native_terminal {
        if let Some(text) = read_if(&root(ctx).join(format!("{name}.native"))).await {
            if let Ok(native) = serde_json::from_str::<CodexSession>(&text) {
                if native.pid == s.pid && native.sid == s.sid { return Some(native); }
            }
        }
    }
    Some(s)
}

/// One dashboard-started Codex thread, as Codex's own state database records it.
#[derive(Debug, Clone, Deserialize)]
pub struct Thread {
    #[serde(default)]
    rollout_path: Option<PathBuf>,
    #[serde(default)]
    display_name: Option<String>,
    sid: String,
    dir: String,
    ts: f64,
    model: Option<String>,
    effort: Option<String>,
}

async fn threads(ctx: &Ctx) -> Result<Vec<Thread>, String> {
    let mut rows: Vec<Thread> = serde_json::from_value(query(ctx, "--threads").await?)
        .map_err(|e| format!("Codex threads: {e}"))?;
    for thread in &mut rows {
        if assert_valid_sid(&thread.sid).is_ok() {
            thread.display_name = read_if(&root(ctx).join(format!("{}.name", thread.sid))).await;
        }
    }
    Ok(rows)
}

pub async fn link(ctx: &Ctx, name: &str) -> Option<String> {
    read_if(&root(ctx).join(format!("{name}.link"))).await
}

pub async fn launch(
    ctx: &Arc<Ctx>,
    dir: &str,
    model: Option<&str>,
    effort: &str,
    previous: Option<CodexSession>,
) -> Result<String, Refused> {
    for program in ["codex", "node"] {
        if !has_binary(&ctx.host.path, program) {
            return Err(Refused::BadRequest(format!(
                "{program} not installed or not on the agent's PATH"
            )));
        }
    }
    // Recheck credentials on every launch/resume; the picker catalog may be cached.
    let choices = discover_models(ctx).await?;
    if choices.is_empty() {
        return Err(Refused::BadRequest(
            "Codex unavailable: run codex login or check its provider configuration".into(),
        ));
    }
    let version = ctx.runner.run_checked("codex", &["--version"], "Codex version")
        .await.map_err(Refused::Failed)?;
    let numbers = version.trim().strip_prefix("codex-cli ").unwrap_or_default()
        .split('.').map(str::parse::<u32>).collect::<Result<Vec<_>, _>>();
    if !matches!(numbers.as_deref(), Ok([major, minor, patch]) if (*major, *minor, *patch) >= (0, 162, 1)) {
        return Err(Refused::BadRequest("Native terminals require Codex CLI 0.162.1 or later; update Codex on the agent host.".into()));
    }
    // A resume that keeps the saved pair is not re-checked: the catalog may
    // have dropped the model since, and that must not strand the session.
    let changed = previous.as_ref().is_none_or(|p| {
        p.model.as_deref() != model || p.effort.as_deref().unwrap_or("medium") != effort
    });
    let choice = if changed {
        if let Some(model) = model {
            let choice = choices
                .into_iter()
                .find(|m| m.model == model)
                .ok_or_else(|| Refused::BadRequest(format!("unknown Codex model: {model}")))?;
            if !choice.efforts.iter().any(|e| e == effort) {
                return Err(Refused::BadRequest(format!(
                    "unsupported effort for {}: {effort}",
                    choice.label
                )));
            }
            Some(choice)
        } else {
            assert_effort(effort)?;
            if effort == "max" {
                return Err(Refused::BadRequest(
                    "Choose a GPT model to select its supported efforts".into(),
                ));
            }
            None
        }
    } else {
        None
    };
    if !tokio::fs::metadata(dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(Refused::BadRequest(format!("not a directory: {dir}")));
    }
    let absolute = std::path::absolute(dir).map_err(|e| Refused::BadRequest(e.to_string()))?;
    let dir = absolute
        .to_str()
        .ok_or_else(|| Refused::BadRequest("directory is not UTF-8".into()))?;
    for program in ["node", "codex", "claude"] {
        let version = ctx
            .runner
            .run_checked(program, &["--version"], program)
            .await
            .map_err(Refused::Failed)?;
        if program == "node"
            && version
                .trim()
                .trim_start_matches('v')
                .split('.')
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0)
                < 22
        {
            return Err(Refused::Failed(
                "Codex bridge requires Node.js 22 or newer".into(),
            ));
        }
    }
    let base = root(ctx);
    tokio::fs::create_dir_all(&base)
        .await
        .map_err(|e| Refused::Failed(e.to_string()))?;
    let name = tmux_name(dir);
    let script = base.join("bridge.mjs");
    write_atomic(
        &script,
        include_str!("codex-remote.mjs"),
        &format!(".{name}.tmp"),
    )
    .await
    .map_err(|e| Refused::Failed(e.to_string()))?;
    // Each pane needs its own runner directory: Claude otherwise reuses the
    // previous RC session for the cwd, attaching a fresh Codex thread to it.
    let control = base.join("controls").join(&name);
    tokio::fs::create_dir_all(&control)
        .await
        .map_err(|e| Refused::Failed(e.to_string()))?;
    let state = base.join(format!("{name}.json"));
    let display_name = match previous.as_ref().and_then(|s| s.display_name.clone()) {
        Some(name) => name,
        None => {
            let sid = previous.as_ref().and_then(|s| s.sid.clone())
                .map(Ok)
                .unwrap_or_else(super::names::new_sid)?;
            super::names::session_name("Codex", dir, &sid)
        }
    };
    let initial = CodexSession {
        native_terminal: true,
        display_name: Some(display_name),
        dir: dir.into(),
        effort: Some(effort.into()),
        model: model.map(str::to_owned),
        model_label: choice
            .map(|m| m.label)
            .or_else(|| previous.as_ref().and_then(|s| s.model_label.clone())),
        working: false,
        ..previous.unwrap_or_default()
    };
    write_atomic(&state, &serde_json::to_string(&initial).unwrap(), ".tmp")
        .await
        .map_err(|e| Refused::Failed(e.to_string()))?;
    super::spawn::trust_dir(
        &super::spawn::claude_json_path(&ctx.claude_dir),
        &control.to_string_lossy(),
    )
    .await
    .map_err(|e| Refused::Failed(format!("Codex runner trust write: {e}")))?;
    let agent = std::env::current_exe().map_err(|e| Refused::Failed(format!("agent executable: {e}")))?;
    let agent_env = format!("CDASH_CODEX_AGENT={}", agent.display());
    ctx.runner
        .run_checked(
            "tmux",
            &[
                "new-session",
                "-d",
                "-e",
                &agent_env,
                "-s",
                &name,
                "-c",
                dir,
                "node",
                &script.to_string_lossy(),
                "--state",
                &state.to_string_lossy(),
                "--name",
                &name,
            ],
            "tmux new-session",
        )
        .await
        .map_err(Refused::Failed)?;
    ctx.host.log.push(format!("launch Codex → {name}"));
    Ok(name)
}

pub async fn resume(
    ctx: &Arc<Ctx>,
    sid: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<Option<String>, Refused> {
    assert_valid_sid(sid)?;
    // Fresh, not the poll's cache: this decides which model the thread resumes on.
    let Some(t) = threads(ctx).await.map_err(Refused::Failed)?.into_iter().find(|t| t.sid == sid)
    else {
        return Ok(None);
    };
    if model.is_some_and(|m| !is_codex_model(m)) {
        return Err(Refused::BadRequest(format!(
            "not a Claude session: cannot resume with {}",
            model.unwrap_or_default()
        )));
    }
    // Bare `codex` names no model, so it keeps the one Codex recorded.
    let model = model.and_then(|m| m.strip_prefix("codex:")).map(str::to_owned).or_else(|| t.model.clone());
    let effort = effort.map(str::to_owned).or_else(|| t.effort.clone()).unwrap_or_else(|| "medium".into());
    let dir = t.dir.clone();
    let previous = CodexSession {
        display_name: t.display_name,
        dir: t.dir,
        sid: Some(t.sid),
        ts: t.ts,
        model: t.model,
        effort: t.effort,
        ..Default::default()
    };
    let name = launch(ctx, &dir, model.as_deref(), &effort, Some(previous)).await?;
    ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);
    Ok(Some(name))
}

/// How long the poll reuses one read of Codex's thread list. Each read starts
/// node, which is real work on a phone host at a 4s poll.
const THREADS_TTL: Duration = Duration::from_secs(10);

pub async fn history(ctx: &Ctx) -> Vec<Resumable> {
    let rows = {
        let mut cache = ctx.codex_threads.lock().await;
        match cache.as_ref().filter(|(when, _)| when.elapsed() < THREADS_TTL) {
            Some((_, rows)) => rows.clone(),
            None => {
                // An unreadable Codex means no Codex rows, and is cached like a
                // good answer so a broken install is not retried every poll.
                let rows = threads(ctx).await.unwrap_or_default();
                *cache = Some((Instant::now(), rows.clone()));
                rows
            }
        }
    };
    // Labels only from a catalog already fetched: the poll must never wait on one.
    let catalog = ctx
        .codex_models
        .try_lock()
        .ok()
        .and_then(|c| c.as_ref().map(|(_, m)| m.clone()))
        .unwrap_or_default();
    rows.into_iter()
        .map(|t| {
            let label = t
                .model
                .as_ref()
                .and_then(|m| catalog.iter().find(|c| &c.model == m))
                .map(|c| c.label.clone());
            Resumable {
                display_name: Some(t.display_name.unwrap_or_else(||
                    super::names::session_name("Codex", &t.dir, &t.sid))),
                sid: t.sid,
                dir: Some(t.dir),
                ts: t.ts,
                branch: None,
                // Bare `codex` still marks the provider when no model was recorded.
                model_id: Some(t.model.as_ref().map_or("codex".into(), |m| format!("codex:{m}"))),
                model: label.or(t.model).or_else(|| Some("Codex".into())),
                effort: t.effort,
                prompts: Vec::new(),
                summary: None,
                source_file: t.rollout_path,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{cmd::Runner, init::Host, log::LogBuffer, sample::Sampler};
    use std::os::unix::fs::PermissionsExt;

    const SID: &str = "01234567-1234-1234-1234-0123456789ab";
    const CATALOG: &str = r#"[{"value":"codex:gpt-b","model":"gpt-b","label":"GPT B","efforts":["low","high"],"defaultEffort":"low"}]"#;

    fn row(dir: &std::path::Path) -> String {
        format!(
            r#"[{{"sid":"{SID}","dir":"{}","title":"Saved","ts":1792000000000,"model":"gpt-a","effort":"low"}}]"#,
            dir.display()
        )
    }

    /// A host whose `node` answers `--version`, `--threads` (from `threads.json`,
    /// which a test can rewrite) and `--models`; `codex`/`claude` only report a
    /// version. `tmux` records every call. `with_codex = false` leaves `codex` out.
    fn host_in(tag: &str, with_codex: bool, catalog: &str) -> (PathBuf, Arc<Ctx>) {
        let dir = std::env::temp_dir().join(format!("cdash-codex-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("threads.json"), row(&dir)).unwrap();
        let mut scripts = vec![
            ("tmux", format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/tmux-args'\n", dir.display())),
            (
                "node",
                format!(
                    "#!/bin/sh\nif [ \"$1\" = --version ]; then echo v22.1.0; exit; fi\nfor a; do last=$a; done\n\
                     if [ \"$last\" = --threads ]; then read -r line < '{}/threads.json'; echo \"$line\"; else echo '{catalog}'; fi\n",
                    dir.display()
                ),
            ),
            ("claude", "#!/bin/sh\necho 1.0.0\n".to_string()),
        ];
        if with_codex {
            scripts.push(("codex", "#!/bin/sh\necho codex-cli 0.162.1\n".to_string()));
        }
        for (name, body) in scripts {
            let f = dir.join("bin").join(name);
            std::fs::write(&f, body).unwrap();
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let log = Arc::new(LogBuffer::new());
        let path = dir.join("bin").to_string_lossy().into_owned();
        let ctx = Arc::new(Ctx::new(
            Host {
                runner: Runner::new(path.clone(), log.clone()),
                log,
                path,
                sampler: std::sync::Mutex::new(Sampler::new()),
            },
            dir.join(".claude"),
            None,
        ));
        (dir, ctx)
    }

    fn codex_ctx(tag: &str) -> (PathBuf, Arc<Ctx>) {
        host_in(tag, true, CATALOG)
    }

    fn saved_state(dir: &std::path::Path) -> CodexSession {
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        let state = args.split("--state ").nth(1).unwrap().split_whitespace().next().unwrap();
        serde_json::from_str(&std::fs::read_to_string(state).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn snapshot_uses_native_metadata_only_for_the_matching_pane_and_thread() {
        let (dir, ctx) = codex_ctx("native-snapshot");
        std::fs::create_dir_all(root(&ctx)).unwrap();
        let mut state = CodexSession { native_terminal: true, pid: Some(42), sid: Some(SID.into()), dir: dir.to_string_lossy().into_owned(), model: Some("old".into()), ..Default::default() };
        std::fs::write(root(&ctx).join("cdash-test.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        state.model = Some("new".into()); state.effort = Some("high".into()); state.working = true;
        let path = root(&ctx).join("cdash-test.native");
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        let native = snapshot(&ctx, "cdash-test", 42).await.unwrap();
        assert_eq!(native.model.as_deref(), Some("new"));
        assert!(native.working);
        assert!(snapshot(&ctx, "cdash-test", 43).await.is_none());
        state.sid = Some("other-thread".into());
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(snapshot(&ctx, "cdash-test", 42).await.unwrap().model.as_deref(), Some("old"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn history_lists_codex_threads_with_their_own_model_and_effort() {
        let (dir, ctx) = codex_ctx("history");
        let h = history(&ctx).await;
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].sid, SID);
        assert_eq!(h[0].model_id.as_deref(), Some("codex:gpt-a"));
        assert_eq!(h[0].effort.as_deref(), Some("low"));
        assert_eq!(h[0].display_name, Some(super::super::names::session_name("Codex", dir.to_str().unwrap(), SID)));
        // Catalog not fetched yet: the chip falls back to the raw model id.
        assert_eq!(h[0].model.as_deref(), Some("gpt-a"));
    }

    #[tokio::test]
    async fn history_labels_the_model_from_a_warm_catalog_and_marks_a_modelless_thread_as_codex() {
        let (dir, ctx) = codex_ctx("labels");
        *ctx.codex_models.lock().await = Some((
            Instant::now(),
            vec![ModelChoice {
                value: "codex:gpt-a".into(),
                model: "gpt-a".into(),
                label: "GPT A".into(),
                efforts: vec!["low".into()],
                default_effort: "low".into(),
            }],
        ));
        assert_eq!(history(&ctx).await[0].model.as_deref(), Some("GPT A"));
        std::fs::write(
            dir.join("threads.json"),
            format!(r#"[{{"sid":"{SID}","dir":"/tmp","title":"t","ts":1,"model":null,"effort":null}}]"#),
        )
        .unwrap();
        ctx.codex_threads.lock().await.take();
        let h = history(&ctx).await;
        assert_eq!((h[0].model.as_deref(), h[0].model_id.as_deref()), (Some("Codex"), Some("codex")));
    }

    #[tokio::test]
    async fn history_is_cached_briefly_so_the_poll_does_not_spawn_node_every_time() {
        let (dir, ctx) = codex_ctx("cache");
        assert_eq!(history(&ctx).await.len(), 1);
        std::fs::write(dir.join("threads.json"), "[]").unwrap();
        assert_eq!(history(&ctx).await.len(), 1, "within the TTL the first answer stands");
        ctx.codex_threads.lock().await.take();
        assert!(history(&ctx).await.is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_codex_state_hides_codex_threads_without_failing_the_list() {
        let (dir, ctx) = codex_ctx("unreadable");
        std::fs::write(dir.join("threads.json"), r#"{"error":"Codex threads unavailable"}"#).unwrap();
        assert!(history(&ctx).await.is_empty());
    }

    #[tokio::test]
    async fn resume_applies_a_chosen_model_and_effort_to_the_codex_session() {
        let (dir, ctx) = codex_ctx("pick");
        std::fs::create_dir_all(root(&ctx)).unwrap();
        let display_name = "[Codex] saved-project · elephant";
        std::fs::write(root(&ctx).join(format!("{SID}.name")), display_name).unwrap();
        assert_eq!(history(&ctx).await[0].display_name.as_deref(), Some(display_name));
        resume(&ctx, SID, Some("codex:gpt-b"), Some("high")).await.unwrap();
        let s = saved_state(&dir);
        assert_eq!(s.display_name.as_deref(), Some(display_name));
        assert_eq!((s.model.as_deref(), s.effort.as_deref()), (Some("gpt-b"), Some("high")));
        assert_eq!(s.model_label.as_deref(), Some("GPT B"));
        assert_eq!(s.sid.as_deref(), Some(SID), "the thread being resumed must survive");
        assert_eq!(serde_json::to_value(&s).unwrap()["native_terminal"], true);
        assert!(std::fs::read_to_string(dir.join("tmux-args")).unwrap().contains("CDASH_CODEX_AGENT="));
    }

    #[tokio::test]
    async fn an_old_codex_version_is_refused_before_creating_a_session() {
        let (dir, ctx) = codex_ctx("old-cli");
        std::fs::write(dir.join("bin/codex"), "#!/bin/sh\necho codex-cli 0.161.0\n").unwrap();
        let error = launch(&ctx, dir.to_str().unwrap(), Some("gpt-b"), "low", None).await.unwrap_err();
        assert!(matches!(error, Refused::BadRequest(ref m) if m.contains("0.162.1")));
        assert!(!dir.join("tmux-args").exists());
        assert!(!root(&ctx).exists());
    }

    #[tokio::test]
    async fn resume_with_no_choice_keeps_codexs_model_even_if_the_catalog_dropped_it() {
        let (dir, ctx) = codex_ctx("keep");
        resume(&ctx, SID, None, None).await.unwrap();
        let s = saved_state(&dir);
        assert_eq!((s.model.as_deref(), s.effort.as_deref()), (Some("gpt-a"), Some("low")));
        assert!(!dir.join(".claude/cdash-codex/history").exists(), "the bridge keeps no history copy");
    }

    #[tokio::test]
    async fn resume_reads_codex_fresh_rather_than_from_the_poll_cache() {
        let (dir, ctx) = codex_ctx("fresh");
        assert_eq!(history(&ctx).await.len(), 1);
        std::fs::write(
            dir.join("threads.json"),
            row(&dir).replace("gpt-a", "gpt-moved-on"),
        )
        .unwrap();
        resume(&ctx, SID, None, None).await.unwrap();
        assert_eq!(saved_state(&dir).model.as_deref(), Some("gpt-moved-on"));
    }

    #[tokio::test]
    async fn resume_of_a_sid_codex_does_not_know_falls_through() {
        let (dir, ctx) = codex_ctx("unknown");
        let other = "99999999-1234-1234-1234-0123456789ab";
        assert!(resume(&ctx, other, None, None).await.unwrap().is_none());
        assert!(!dir.join("tmux-args").exists());
    }

    #[tokio::test]
    async fn resume_rejects_a_foreign_unknown_or_unsupported_pick_before_spawning() {
        let (dir, ctx) = codex_ctx("reject");
        for (model, effort) in [
            (Some("sonnet"), None),
            (Some("codex:nope"), None),
            (Some("codex:gpt-b"), Some("max")),
        ] {
            let e = resume(&ctx, SID, model, effort).await.unwrap_err();
            assert!(matches!(e, Refused::BadRequest(_)), "{model:?} {effort:?}: {e:?}");
        }
        assert!(!dir.join("tmux-args").exists());
    }

    #[tokio::test]
    async fn unavailable_codex_does_not_block_claude_or_spawn_phantom_panes() {
        for installed in [false, true] {
            // Codex signed out: node answers every catalog query with an empty list.
            let (dir, ctx) = host_in(&format!("signedout-{installed}"), installed, "[]");
            assert!(models(&ctx).await.unwrap().is_empty());
            assert!(super::super::usage::fetch_codex_usage(&ctx).await.unwrap().is_empty());
            let start = Instant::now();
            assert!(ctx.codex_usage.get_codex(&ctx).is_none());
            assert!(start.elapsed() < Duration::from_secs(1));
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Some(tiles) = ctx.codex_usage.get_codex(&ctx) {
                        assert!(tiles.is_empty());
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(ctx.host.log.lines().is_empty(), "unavailable optional Codex should not log failures");
            // A previously signed-in picker must not bypass the fresh launch check.
            *ctx.codex_models.lock().await = Some((
                Instant::now(),
                vec![ModelChoice {
                    value: "codex:gpt-check".into(),
                    model: "gpt-check".into(),
                    label: "GPT Check".into(),
                    efforts: vec!["low".into()],
                    default_effort: "low".into(),
                }],
            ));
            let err = launch(&ctx, dir.to_str().unwrap(), Some("gpt-check"), "low", None)
                .await
                .unwrap_err();
            let reason = if installed { "codex login" } else { "not installed" };
            assert!(matches!(err, Refused::BadRequest(ref m) if m.contains(reason)));
            assert!(!dir.join(".claude/cdash-codex").exists());
            if installed {
                // Codex's thread stays listed; resume refuses before creating a pane.
                assert_eq!(history(&ctx).await.len(), 1);
                let err = resume(&ctx, SID, None, None).await.unwrap_err();
                assert!(matches!(err, Refused::BadRequest(ref m) if m.contains(reason)));
            } else {
                // Without the codex binary its threads cannot be resumed, so they are not offered.
                assert!(history(&ctx).await.is_empty());
                assert!(resume(&ctx, SID, None, None).await.unwrap().is_none());
            }
            assert!(!dir.join("tmux-args").exists());
            assert!(super::super::spawn::launch_session(&ctx, dir.to_str().unwrap(), "sonnet", "medium")
                .await
                .is_ok());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

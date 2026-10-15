//! Managed Codex sessions share the tmux lifecycle; their bridge owns metadata.
use super::ctx::Ctx;
use super::fsio::{read_if, write_atomic};
use super::sessions::Resumable;
use super::spawn::tmux_name;
use super::validate::{assert_effort, assert_valid_sid, Refused};
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
        .run_checked(
            "node",
            &[
                "--input-type=module",
                "-e",
                include_str!("codex-remote.mjs"),
                "--",
                mode,
            ],
            &format!("Codex metadata {mode}"),
        )
        .await?;
    serde_json::from_str(&json).map_err(|_| "Codex metadata returned invalid JSON".into())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexSession {
    pub dir: String,
    pub pid: Option<i32>,
    pub sid: Option<String>,
    #[serde(default)]
    pub working: bool,
    pub last_message: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub ts: f64,
    pub effort: Option<String>,
    pub model: Option<String>,
    pub model_label: Option<String>,
}

fn root(ctx: &Ctx) -> PathBuf {
    ctx.claude_dir.join("cdash-codex")
}

pub async fn snapshot(ctx: &Ctx, name: &str, pid: i32) -> Option<CodexSession> {
    let s: CodexSession =
        serde_json::from_str(&read_if(&root(ctx).join(format!("{name}.json"))).await?).ok()?;
    // An archived snapshot must not claim a later pane that reused its name.
    (s.pid == Some(pid)).then_some(s)
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
    let choice = if previous.is_none() {
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
    tokio::fs::create_dir_all(base.join("history"))
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
    let initial = CodexSession {
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
    ctx.runner
        .run_checked(
            "tmux",
            &[
                "new-session",
                "-d",
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

pub async fn resume(ctx: &Arc<Ctx>, sid: &str) -> Result<Option<String>, Refused> {
    assert_valid_sid(sid)?;
    let Some(txt) = read_if(&root(ctx).join("history").join(format!("{sid}.json"))).await else {
        return Ok(None);
    };
    let s: CodexSession = serde_json::from_str(&txt).map_err(|e| Refused::Failed(e.to_string()))?;
    let name = launch(
        ctx,
        &s.dir.clone(),
        s.model.clone().as_deref(),
        s.effort.clone().as_deref().unwrap_or("medium"),
        Some(s),
    )
    .await?;
    ctx.purged
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(sid);
    Ok(Some(name))
}

pub async fn history(ctx: &Ctx) -> Vec<Resumable> {
    let mut result = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(root(ctx).join("history")).await else {
        return result;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Some(txt) = read_if(&entry.path()).await else {
            continue;
        };
        let Ok(s) = serde_json::from_str::<CodexSession>(&txt) else {
            continue;
        };
        if s.title.is_empty() {
            continue;
        }
        let Some(sid) = s.sid else { continue };
        result.push(Resumable {
            sid,
            dir: Some(s.dir),
            ts: s.ts,
            branch: None,
            title: s.title,
            model: s.model_label.or(s.model).or_else(|| Some("Codex".into())),
            effort: s.effort,
            prompts: Vec::new(),
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{cmd::Runner, init::Host, log::LogBuffer, sample::Sampler};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn unavailable_codex_does_not_block_claude_or_spawn_phantom_panes() {
        for installed in [false, true] {
            let dir = std::env::temp_dir()
                .join(format!("cdash-no-codex-{installed}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            // Isolated PATH: optional Codex is either absent or reports a signed-out catalog.
            let tmux = dir.join("bin/tmux");
            std::fs::write(
                &tmux,
                format!("#!/bin/sh\necho called >> '{}/tmux-args'\n", dir.display()),
            )
            .unwrap();
            std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
            for program in ["node", "codex"] {
                if program == "codex" && !installed {
                    continue;
                }
                let file = dir.join("bin").join(program);
                std::fs::write(&file, "#!/bin/sh\necho '[]'\n").unwrap();
                std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let log = Arc::new(LogBuffer::new());
            let path = dir.join("bin").to_string_lossy().into_owned();
            let ctx = Arc::new(Ctx::new(
                Host {
                    runner: Runner::new(path.clone(), log.clone()),
                    log: log.clone(),
                    path,
                    sampler: std::sync::Mutex::new(Sampler::new()),
                },
                dir.join(".claude"),
                None,
            ));
            assert!(models(&ctx).await.unwrap().is_empty());
            assert!(super::super::usage::fetch_codex_usage(&ctx)
                .await
                .unwrap()
                .is_empty());
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
            assert!(
                log.lines().is_empty(),
                "unavailable optional Codex should not log failures"
            );
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
            let reason = if installed {
                "codex login"
            } else {
                "not installed"
            };
            assert!(matches!(err, Refused::BadRequest(ref message) if message.contains(reason)));
            assert!(!dir.join(".claude/cdash-codex").exists());
            // Old Codex history remains visible; resume refuses before creating a pane.
            let sid = "01234567-1234-1234-1234-0123456789ab";
            let history_dir = dir.join(".claude/cdash-codex/history");
            std::fs::create_dir_all(&history_dir).unwrap();
            let state = CodexSession {
                sid: Some(sid.into()),
                dir: dir.to_string_lossy().into_owned(),
                title: "Saved session".into(),
                effort: Some("low".into()),
                model: Some("gpt-check".into()),
                ..Default::default()
            };
            std::fs::write(
                history_dir.join(format!("{sid}.json")),
                serde_json::to_string(&state).unwrap(),
            )
            .unwrap();
            assert_eq!(history(&ctx).await.len(), 1);
            let err = resume(&ctx, sid).await.unwrap_err();
            assert!(matches!(err, Refused::BadRequest(ref message) if message.contains(reason)));
            assert!(!dir.join("tmux-args").exists());
            assert!(super::super::spawn::launch_session(
                &ctx,
                dir.to_str().unwrap(),
                "sonnet",
                "medium"
            )
            .await
            .is_ok());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

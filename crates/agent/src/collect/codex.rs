//! Managed Codex sessions share the tmux lifecycle; their bridge owns metadata.
use super::ctx::Ctx;
use super::fsio::{read_if, write_atomic};
use super::sessions::Resumable;
use super::spawn::tmux_name;
use super::validate::{assert_effort, assert_valid_sid, Refused};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

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
    effort: &str,
    previous: Option<CodexSession>,
) -> Result<String, Refused> {
    assert_effort(effort)?;
    if effort == "max" {
        return Err(Refused::BadRequest(
            "Codex effort: choose low, medium, high or xhigh".into(),
        ));
    }
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
            title: format!(
                "Codex: {}",
                if s.title.is_empty() {
                    "(untitled)"
                } else {
                    &s.title
                }
            ),
            prompts: Vec::new(),
        });
    }
    result
}

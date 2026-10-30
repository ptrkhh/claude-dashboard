//! Dashboard-owned OpenCode sessions use one private server per native pane.
use super::ctx::Ctx;
use super::fsio::{read_if, write_atomic};
use super::sessions::Resumable;
use super::validate::{assert_kill_name, assert_opencode_sid, assert_path, Refused};
use crate::host::probe::has_binary;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChoice {
    pub value: String,
    pub model: String,
    pub label: String,
    pub provider: String,
    pub variants: Vec<String>,
    pub default_variant: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenCodeSession {
    pub dir: String,
    pub sid: Option<String>,
    pub pid: Option<i32>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_label: Option<String>,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub native_terminal: bool,
    #[serde(default)]
    pub working: bool,
    #[serde(default)]
    pub rc_unavailable: bool,
    #[serde(default)]
    pub last_message: Option<String>,
    #[serde(default)]
    pub ts: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Thread {
    sid: String,
    dir: String,
    ts: f64,
    display_name: Option<String>,
    model: Option<String>,
    model_label: Option<String>,
    variant: Option<String>,
    #[serde(default)]
    prompts: Vec<String>,
    source_file: Option<PathBuf>,
}

static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn root(ctx: &Ctx) -> PathBuf {
    ctx.claude_dir.join("cdash-opencode")
}

async fn private_write(path: &Path, contents: &str, suffix: &str) -> Result<(), Refused> {
    write_atomic(path, contents, suffix)
        .await
        .map_err(|e| Refused::Failed(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|e| Refused::Failed(e.to_string()))?;
    }
    Ok(())
}

async fn install(ctx: &Ctx) -> Result<PathBuf, Refused> {
    let _guard = INSTALL.lock().await;
    let base = root(ctx);
    tokio::fs::create_dir_all(&base)
        .await
        .map_err(|e| Refused::Failed(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| Refused::Failed(e.to_string()))?;
    }
    for (name, contents) in [
        ("opencode-api.mjs", include_str!("opencode-api.mjs")),
        ("claude-remote.mjs", include_str!("claude-remote.mjs")),
        ("opencode-remote.mjs", include_str!("opencode-remote.mjs")),
    ] {
        let file = base.join(name);
        if read_if(&file).await.as_deref() != Some(contents) {
            private_write(&file, contents, &format!(".{}.tmp", std::process::id())).await?;
        }
    }
    Ok(base.join("opencode-remote.mjs"))
}

async fn versions(ctx: &Ctx) -> Result<(), Refused> {
    for program in ["node", "opencode"] {
        if !has_binary(&ctx.host.path, program) {
            return Err(Refused::BadRequest(format!(
                "{program} not installed or not on the agent's PATH"
            )));
        }
    }
    let node = ctx
        .runner
        .run_checked("node", &["--version"], "Node version")
        .await
        .map_err(Refused::Failed)?;
    if node
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
        < 22
    {
        return Err(Refused::BadRequest(
            "OpenCode bridge requires Node.js 22 or newer".into(),
        ));
    }
    let version = ctx
        .runner
        .run_checked("opencode", &["--version"], "OpenCode version")
        .await
        .map_err(Refused::Failed)?;
    let numbers = version
        .trim()
        .strip_prefix("opencode v")
        .unwrap_or_default()
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>();
    if !matches!(numbers.as_deref(), Ok([2, minor, patch]) if (*minor, *patch) >= (1, 4)) {
        return Err(Refused::BadRequest(
            "OpenCode 2.1.4 or newer within major version 2 is required".into(),
        ));
    }
    Ok(())
}

async fn query(ctx: &Ctx, args: &[&str]) -> Result<serde_json::Value, Refused> {
    let script = install(ctx).await?;
    let script = script.to_string_lossy().into_owned();
    let mut command = vec![script.as_str()];
    command.extend_from_slice(args);
    let output = ctx
        .runner
        .run_checked_with_timeout(
            "node",
            &command,
            "OpenCode metadata",
            Duration::from_secs(20),
        )
        .await
        .map_err(Refused::Failed)?;
    serde_json::from_str(&output)
        .map_err(|_| Refused::Failed("OpenCode metadata returned invalid JSON".into()))
}

async fn discover_models(ctx: &Ctx, dir: &str) -> Result<Vec<ModelChoice>, Refused> {
    serde_json::from_value(query(ctx, &["--models", "--dir", dir]).await?)
        .map_err(|_| Refused::Failed("Invalid OpenCode model catalog".into()))
}

pub async fn models(ctx: &Ctx, dir: &str) -> Result<Vec<ModelChoice>, Refused> {
    assert_path(dir)?;
    if !tokio::fs::metadata(dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(Refused::BadRequest(format!("not a directory: {dir}")));
    }
    if !has_binary(&ctx.host.path, "node") || !has_binary(&ctx.host.path, "opencode") {
        return Ok(Vec::new());
    }
    let mut cache = ctx.opencode_models.lock().await;
    if let Some((when, values)) = cache
        .get(dir)
        .filter(|(when, _)| when.elapsed() < Duration::from_secs(60))
    {
        let _ = when;
        return Ok(values.clone());
    }
    versions(ctx).await?;
    let choices = discover_models(ctx, dir).await?;
    if cache.len() >= 32 && !cache.contains_key(dir) {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (when, _))| *when)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(dir.to_owned(), (Instant::now(), choices.clone()));
    Ok(choices)
}

async fn threads(ctx: &Ctx) -> Result<Vec<Thread>, Refused> {
    if !has_binary(&ctx.host.path, "node") || !has_binary(&ctx.host.path, "opencode") {
        return Ok(Vec::new());
    }
    let base = root(ctx);
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    versions(ctx).await?;
    serde_json::from_value(
        query(
            ctx,
            &[
                "--threads",
                "--root",
                &base.to_string_lossy(),
                "--limit",
                "100",
            ],
        )
        .await?,
    )
    .map_err(|_| Refused::Failed("Invalid OpenCode history".into()))
}

pub async fn history(ctx: &Ctx, limit: usize) -> Vec<Resumable> {
    let rows = {
        let mut cache = ctx.opencode_threads.lock().await;
        if let Some((_, rows)) = cache
            .as_ref()
            .filter(|(when, _)| when.elapsed() < Duration::from_secs(10))
        {
            rows.clone()
        } else {
            let rows = threads(ctx).await.unwrap_or_default();
            *cache = Some((Instant::now(), rows.clone()));
            rows
        }
    };
    rows.into_iter()
        .filter(|t| assert_opencode_sid(&t.sid).is_ok())
        .take(limit.min(100))
        .map(|t| Resumable {
            display_name: Some(
                t.display_name
                    .unwrap_or_else(|| super::names::session_name("OpenCode", &t.dir, &t.sid)),
            ),
            sid: t.sid,
            dir: Some(t.dir),
            ts: t.ts,
            branch: None,
            prompts: t.prompts,
            model_id: t.model.as_ref().map(|m| format!("opencode:{m}")),
            model: t.model_label.or(t.model),
            effort: None,
            variant: t.variant,
            summary: None,
            source_file: t.source_file,
        })
        .collect()
}

pub async fn snapshot(ctx: &Ctx, name: &str, pid: i32) -> Option<OpenCodeSession> {
    assert_kill_name(name).ok()?;
    let base = root(ctx);
    let state: OpenCodeSession =
        serde_json::from_str(&read_if(&base.join(format!("{name}.json"))).await?).ok()?;
    let native: OpenCodeSession =
        serde_json::from_str(&read_if(&base.join(format!("{name}.native"))).await?).ok()?;
    if native.pid != Some(pid)
        || native.sid.is_none()
        || native.sid != state.sid
        || !native.native_terminal
    {
        return None;
    }
    Some(native)
}

pub async fn link(ctx: &Ctx, name: &str) -> Option<String> {
    assert_kill_name(name).ok()?;
    read_if(&root(ctx).join(format!("{name}.link"))).await
}

pub async fn launch(
    ctx: &Arc<Ctx>,
    dir: &str,
    model: &str,
    variant: Option<&str>,
    previous: Option<OpenCodeSession>,
) -> Result<String, Refused> {
    assert_path(dir)?;
    if !tokio::fs::metadata(dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(Refused::BadRequest(format!("not a directory: {dir}")));
    }
    versions(ctx).await?;
    let variant = variant.filter(|v| !v.is_empty());
    let changed = previous
        .as_ref()
        .is_none_or(|p| p.model.as_deref() != Some(model) || p.variant.as_deref() != variant);
    let choice = if changed {
        let choice = discover_models(ctx, dir)
            .await?
            .into_iter()
            .find(|m| m.model == model)
            .ok_or_else(|| Refused::BadRequest("Unknown OpenCode model for this project".into()))?;
        if variant.is_some_and(|v| !choice.variants.iter().any(|item| item == v)) {
            return Err(Refused::BadRequest("Unsupported OpenCode variant".into()));
        }
        Some(choice)
    } else {
        None
    };
    let script = install(ctx).await?;
    let base = root(ctx);
    let name = super::spawn::tmux_name(dir);
    let state_path = base.join(format!("{name}.json"));
    let display_name = match previous.as_ref().and_then(|p| p.display_name.clone()) {
        Some(name) => name,
        None => super::names::session_name("OpenCode", dir, &super::names::new_sid()?),
    };
    let state = OpenCodeSession {
        dir: dir.into(),
        display_name: Some(display_name),
        model: Some(model.into()),
        variant: variant.map(str::to_owned),
        model_label: choice
            .map(|m| m.label)
            .or_else(|| previous.as_ref().and_then(|p| p.model_label.clone())),
        native_terminal: true,
        pid: None,
        working: false,
        ..previous.unwrap_or_default()
    };
    private_write(
        &state_path,
        &serde_json::to_string(&state).map_err(|e| Refused::Failed(e.to_string()))?,
        ".tmp",
    )
    .await?;
    let control = base.join("controls").join(&name);
    tokio::fs::create_dir_all(&control)
        .await
        .map_err(|e| Refused::Failed(e.to_string()))?;
    super::spawn::trust_dir(
        &super::spawn::claude_json_path(&ctx.claude_dir),
        &control.to_string_lossy(),
    )
    .await
    .map_err(|e| Refused::Failed(format!("OpenCode runner trust write: {e}")))?;
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
                &state_path.to_string_lossy(),
                "--name",
                &name,
            ],
            "tmux new-session",
        )
        .await
        .map_err(Refused::Failed)?;
    ctx.host.log.push(format!("launch OpenCode → {name}"));
    Ok(name)
}

pub async fn resume(
    ctx: &Arc<Ctx>,
    sid: &str,
    model: Option<&str>,
    variant: Option<&str>,
) -> Result<String, Refused> {
    assert_opencode_sid(sid)?;
    if model.is_some_and(|m| !m.starts_with("opencode:")) {
        return Err(Refused::BadRequest(
            "Cannot resume an OpenCode session with another tool's model".into(),
        ));
    }
    let row = threads(ctx)
        .await?
        .into_iter()
        .find(|row| row.sid == sid)
        .ok_or_else(|| Refused::BadRequest(format!("Unknown OpenCode session: {sid}")))?;
    let old_model = row
        .model
        .clone()
        .ok_or_else(|| Refused::BadRequest("OpenCode session has no model".into()))?;
    let selected = model
        .and_then(|m| m.strip_prefix("opencode:"))
        .unwrap_or(&old_model)
        .to_owned();
    let chosen_variant = variant
        .map(|v| {
            if v.is_empty() {
                None
            } else {
                Some(v.to_owned())
            }
        })
        .unwrap_or_else(|| row.variant.clone());
    let dir = row.dir.clone();
    let previous = OpenCodeSession {
        sid: Some(row.sid),
        dir: row.dir,
        display_name: row.display_name,
        model: Some(old_model),
        model_label: row.model_label,
        variant: row.variant,
        ts: row.ts,
        ..Default::default()
    };
    let name = launch(
        ctx,
        &dir,
        &selected,
        chosen_variant.as_deref(),
        Some(previous),
    )
    .await?;
    ctx.purged
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(sid);
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{cmd::Runner, init::Host, log::LogBuffer, sample::Sampler};
    use std::os::unix::fs::PermissionsExt;

    fn fixture(tag: &str, installed: bool) -> (std::path::PathBuf, Arc<Ctx>) {
        let dir = std::env::temp_dir().join(format!("cdash-opencode-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("catalog.json"), r#"[{"value":"opencode:nvidia/deepseek-ai/deepseek-v4.1-flash","model":"nvidia/deepseek-ai/deepseek-v4.1-flash","label":"OpenCode: DeepSeek","provider":"opencode","variants":["fast","deep"],"defaultVariant":null}]"#).unwrap();
        std::fs::write(dir.join("threads.json"), format!(r#"[{{"sid":"ses_saved","dir":"{}","ts":1000,"display_name":"Saved","model":"nvidia/old-model","model_label":"Old","variant":"deep","prompts":["First"],"source_file":null}}]"#, dir.display())).unwrap();
        let mut programs = vec![
            ("node", format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo v22.1.0; exit; fi\nprintf '%s\\n' \"$*\" >> '{}/calls'\ncase \"$2\" in --models) read -r line < '{}/catalog.json';; --threads) read -r line < '{}/threads.json';; esac\nprintf '%s\\n' \"$line\"\n", dir.display(), dir.display(), dir.display())),
            ("tmux", format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/tmux-args'\n", dir.display())),
            ("claude", "#!/bin/sh\necho 1.0.0\n".to_string()),
        ];
        if installed {
            programs.push(("opencode", "#!/bin/sh\necho opencode v2.1.4\n".to_string()));
        }
        for (name, body) in programs {
            let file = dir.join("bin").join(name);
            std::fs::write(&file, body).unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
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

    #[tokio::test]
    async fn opencode_catalog_keeps_slashes_and_uses_project_cache() {
        let (dir, ctx) = fixture("catalog", true);
        let choices = models(&ctx, dir.to_str().unwrap()).await.unwrap();
        assert_eq!(choices[0].model, "nvidia/deepseek-ai/deepseek-v4.1-flash");
        assert_eq!(choices[0].variants, ["fast", "deep"]);
        models(&ctx, dir.to_str().unwrap()).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn opencode_launch_keeps_full_model_identity_and_validates_variant() {
        let (dir, ctx) = fixture("launch", true);
        let model = "nvidia/deepseek-ai/deepseek-v4.1-flash";
        assert!(
            launch(&ctx, dir.to_str().unwrap(), model, Some("missing"), None)
                .await
                .is_err()
        );
        assert!(!dir.join("tmux-args").exists());
        launch(&ctx, dir.to_str().unwrap(), model, Some("deep"), None)
            .await
            .unwrap();
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        let state_path = args
            .split("--state ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let state: OpenCodeSession =
            serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
        assert_eq!(state.model.as_deref(), Some(model));
        assert_eq!(state.variant.as_deref(), Some("deep"));
        assert_eq!(
            std::fs::metadata(state_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(root(&ctx)).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[tokio::test]
    async fn opencode_models_are_cached_per_directory_with_a_bound() {
        let (dir, ctx) = fixture("cache", true);
        for index in 0..33 {
            let project = dir.join(format!("project-{index}"));
            std::fs::create_dir_all(&project).unwrap();
            assert_eq!(
                models(&ctx, project.to_str().unwrap()).await.unwrap().len(),
                1
            );
        }
        assert_eq!(ctx.opencode_models.lock().await.len(), 32);
        assert_eq!(
            std::fs::read_to_string(dir.join("calls"))
                .unwrap()
                .lines()
                .count(),
            33
        );
    }

    #[tokio::test]
    async fn unsupported_opencode_version_is_rejected_before_tmux() {
        let (dir, ctx) = fixture("version", true);
        for version in ["opencode v1.9.0", "opencode v2.1.3", "opencode v3.0.0"] {
            std::fs::write(
                dir.join("bin/opencode"),
                format!("#!/bin/sh\necho {version}\n"),
            )
            .unwrap();
            assert!(
                launch(&ctx, dir.to_str().unwrap(), "nvidia/model", None, None)
                    .await
                    .is_err()
            );
            assert!(!dir.join("tmux-args").exists());
        }
    }

    #[tokio::test]
    async fn opencode_concurrent_contexts_materialize_scripts_without_corruption() {
        let (dir, first) = fixture("install-race", true);
        let log = Arc::new(LogBuffer::new());
        let path = dir.join("bin").to_string_lossy().into_owned();
        let second = Arc::new(Ctx::new(
            Host {
                runner: Runner::new(path.clone(), log.clone()),
                log,
                path,
                sampler: std::sync::Mutex::new(Sampler::new()),
            },
            first.claude_dir.clone(),
            None,
        ));
        let (a, b) = tokio::join!(
            models(&first, dir.to_str().unwrap()),
            models(&second, dir.to_str().unwrap())
        );
        assert_eq!(a.unwrap().len(), 1);
        assert_eq!(b.unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(root(&first).join("opencode-remote.mjs")).unwrap(),
            include_str!("opencode-remote.mjs")
        );
    }

    #[tokio::test]
    async fn unavailable_opencode_does_not_spawn_a_pane() {
        let (dir, ctx) = fixture("missing", false);
        assert!(models(&ctx, dir.to_str().unwrap())
            .await
            .unwrap()
            .is_empty());
        assert!(
            launch(&ctx, dir.to_str().unwrap(), "nvidia/model", None, None)
                .await
                .is_err()
        );
        assert!(!dir.join("tmux-args").exists());
    }

    #[tokio::test]
    async fn opencode_pane_names_cannot_escape_private_state() {
        let (dir, ctx) = fixture("bad-name", true);
        std::fs::create_dir_all(root(&ctx).join("cdash-x")).unwrap();
        std::fs::write(ctx.claude_dir.join("secret.link"), "outside").unwrap();
        assert!(link(&ctx, "cdash-x/../../secret").await.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn opencode_snapshot_requires_matching_pane_and_session() {
        let (dir, ctx) = fixture("snapshot", true);
        std::fs::create_dir_all(root(&ctx)).unwrap();
        std::fs::write(
            root(&ctx).join("cdash-test.json"),
            r#"{"sid":"ses_a","dir":"/tmp"}"#,
        )
        .unwrap();
        std::fs::write(
            root(&ctx).join("cdash-test.native"),
            r#"{"sid":"ses_a","dir":"/tmp","pid":42,"native_terminal":true}"#,
        )
        .unwrap();
        assert!(snapshot(&ctx, "cdash-test", 42).await.is_some());
        assert!(snapshot(&ctx, "cdash-test", 99).await.is_none());
        std::fs::write(
            root(&ctx).join("cdash-test.json"),
            r#"{"sid":"ses_b","dir":"/tmp"}"#,
        )
        .unwrap();
        assert!(snapshot(&ctx, "cdash-test", 42).await.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn opencode_resume_route_distinguishes_omitted_variant_and_default() {
        let (dir, ctx) = fixture("resume-variant", true);
        std::fs::create_dir_all(root(&ctx)).unwrap();
        std::fs::write(dir.join("catalog.json"), r#"[{"value":"opencode:nvidia/old-model","model":"nvidia/old-model","label":"Old","provider":"opencode","variants":["deep"],"defaultVariant":null}]"#).unwrap();
        let call = |variant: Option<&str>| crate::http::routes::ResumeBody {
            sid: "ses_saved".into(),
            model: None,
            effort: None,
            variant: variant.map(str::to_owned),
        };
        crate::http::routes::post_resume(axum::extract::State(ctx.clone()), axum::Json(call(None)))
            .await
            .ok()
            .unwrap();
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        let path = args
            .lines()
            .last()
            .unwrap()
            .split("--state ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let state: OpenCodeSession =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(state.variant.as_deref(), Some("deep"));
        crate::http::routes::post_resume(axum::extract::State(ctx), axum::Json(call(Some(""))))
            .await
            .ok()
            .unwrap();
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        let path = args
            .lines()
            .last()
            .unwrap()
            .split("--state ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let state: OpenCodeSession =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(state.variant, None);
    }

    #[tokio::test]
    async fn opencode_resume_keeps_missing_model_and_rejects_cross_tool_choices() {
        let (dir, ctx) = fixture("resume", true);
        std::fs::create_dir_all(root(&ctx)).unwrap();
        std::fs::write(dir.join("catalog.json"), "[]\n").unwrap();
        assert!(resume(&ctx, "ses_saved", Some("codex:gpt-x"), None)
            .await
            .is_err());
        assert!(!dir.join("tmux-args").exists());
        resume(&ctx, "ses_saved", None, None).await.unwrap();
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        assert!(args.contains("--state"));
        let state_path = args
            .split("--state ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let state: OpenCodeSession =
            serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
        assert_eq!(state.sid.as_deref(), Some("ses_saved"));
        assert_eq!(state.model.as_deref(), Some("nvidia/old-model"));
        assert_eq!(state.variant.as_deref(), Some("deep"));
        assert!(resume(&ctx, "ses_saved", None, Some("other"))
            .await
            .is_err());
        assert!(resume(&ctx, "ses_unowned", None, None).await.is_err());
    }
}

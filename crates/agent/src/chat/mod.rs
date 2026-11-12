//! Dashboard chat: one hub per chatted session, fed by an engine that reads the
//! tool's own record and types into it. See
//! `docs/superpowers/specs/2026-11-12-chat-core-design.md`.

pub mod claude;
pub mod hub;
pub mod queue;
pub mod tap;
pub mod tmux_io;
pub mod wire;
pub mod ws;

/// Largest chat message, in bytes, after sanitizing.
pub const MAX_TEXT: usize = 32 * 1024;
/// Tool input and output cap, in characters, as the bridges already cap them.
pub const CAP: usize = 65_536;
/// One serialized JSON frame, shared between subscribers without copying.
pub type Frame = std::sync::Arc<str>;

use crate::collect::ctx::Ctx;
use crate::collect::validate::Refused;
use std::sync::Arc;

/// The running hub for a pane, started on first use.
pub async fn hub_for(ctx: &Arc<Ctx>, name: &str) -> Result<hub::Hub, Refused> {
    let mut chats = ctx.chats.lock().await;
    if let Some(h) = chats.get(name).filter(|h| !h.is_closed()) {
        return Ok(h.clone());
    }
    let no_chat = || Refused::BadRequest("this session has no chat".into());
    let (pid, bare) = tmux_io::pane(&ctx.runner, name).await.ok_or_else(no_chat)?;
    if bare {
        return Err(no_chat());
    }
    // Same order as `collect_sessions`: OpenCode, then Codex, else Claude.
    if crate::collect::opencode::snapshot(ctx, name, pid).await.is_some()
        || crate::collect::codex::snapshot(ctx, name, pid).await.is_some()
    {
        return Err(Refused::BadRequest("chat for this tool is not available yet".into()));
    }
    let (engine, rx) = claude::start(Arc::clone(ctx), name.to_string(), pid);
    let hub = hub::Hub::spawn("claude", engine, rx, Arc::clone(&ctx.host.log), name.to_string());
    chats.insert(name.to_string(), hub.clone());
    Ok(hub)
}

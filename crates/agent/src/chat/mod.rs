//! Dashboard chat: one hub per chatted session, fed by an engine that reads the
//! tool's own record and types into it. See
//! `docs/superpowers/specs/2026-11-12-chat-core-design.md`.

pub mod tap;
pub mod wire;

/// Largest chat message, in bytes, after sanitizing.
pub const MAX_TEXT: usize = 32 * 1024;
/// Tool input and output cap, in characters, as the bridges already cap them.
pub const CAP: usize = 65_536;
/// One serialized JSON frame, shared between subscribers without copying.
pub type Frame = std::sync::Arc<str>;

/// A rejected request. The HTTP layer renders this as a 400 with the message
/// as the `error` field, matching `server.js:41`.
#[derive(Debug, Clone, PartialEq)]
pub struct BadRequest(pub String);

/// How a mutating route fails. The distinction is the point: `BadRequest` is
/// the caller's input (400), `Failed` is a subprocess we ran on their behalf
/// (500). Node got this for free — its mutating routes used the *throwing*
/// `run`, not the swallowing `sh` — and collapsing the two is how a `tmux
/// kill-session` that never ran came back as `200 {"ok":true}`.
#[derive(Debug, Clone, PartialEq)]
pub enum Refused {
    BadRequest(String),
    Failed(String),
}

impl From<BadRequest> for Refused {
    fn from(e: BadRequest) -> Self {
        Self::BadRequest(e.0)
    }
}

/// Mirrors `MODELS` (`lib/collect.js:108`).
pub const MODELS: &[&str] = &["sonnet", "opus", "haiku", "fable"];
/// Mirrors `EFFORTS` (`lib/collect.js:109`).
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Mirrors `assertPath` (`server.js:28-30`), extended to the platform's path
/// shapes (spec §4): a drive, a `\\wsl…` share or a `/` path on Windows, `/`
/// only elsewhere. Shape only — routing to a side is `side::side_for`.
pub fn assert_path(p: &str) -> Result<(), BadRequest> {
    if super::side::path_is_valid(p) {
        Ok(())
    } else {
        Err(BadRequest(format!("bad path: {p}")))
    }
}

/// Mirrors the inline guard at `server.js:62`. The value reaches
/// `tmux kill-session -t`, so nothing outside this shape may pass.
///
/// Byte-wise and not a regex: JS's `\w` without the `u` flag is ASCII, and
/// every regex engine here is Unicode-aware by default, so the shape has to
/// say ASCII rather than remember a flag. `bytes()` also means no `^…$`
/// anchor question — there is no line for a newline to start.
pub fn assert_kill_name(name: &str) -> Result<(), BadRequest> {
    let ok = name.strip_prefix("cdash-").is_some_and(|rest| {
        !rest.is_empty()
            && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    });
    if ok {
        Ok(())
    } else {
        Err(BadRequest("bad name".to_string()))
    }
}

/// Mirrors `assertValidSid` (`lib/collect.js:168-170`). A fixed length over
/// bytes, so no anchor can be escaped by a newline and no Unicode digit
/// widens the class.
pub fn assert_valid_sid(sid: &str) -> Result<(), BadRequest> {
    if sid.len() == 36 && sid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        Ok(())
    } else {
        Err(BadRequest(format!("bad sid: {sid}")))
    }
}

pub fn assert_model(model: &str) -> Result<(), BadRequest> {
    if MODELS.contains(&model) {
        Ok(())
    } else {
        Err(BadRequest(format!("bad model: {model}")))
    }
}

pub fn assert_effort(effort: &str) -> Result<(), BadRequest> {
    if EFFORTS.contains(&effort) {
        Ok(())
    } else {
        Err(BadRequest(format!("bad effort: {effort}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_path_requires_an_absolute_path() {
        assert!(assert_path("/home/x").is_ok());
        assert!(assert_path("relative/x").is_err());
        assert!(assert_path("").is_err());
        // A traversal only reaches this guard in relative form: it has no root
        // to confine to, it guards a stored favourite, not a read.
        assert!(assert_path("../../etc/passwd").is_err());
    }

    #[test]
    fn the_kill_target_pattern_is_ascii_only_as_node_s_was() {
        // The regex crate's \\w is Unicode-aware; JS's (no /u flag) is not. A
        // cdash-<non-ASCII> session was created by something other than this
        // dashboard, and Node refused to kill it.
        for name in ["cdash-caf\u{e9}", "cdash-\u{65e5}\u{672c}", "cdash-a\u{301}"] {
            assert!(assert_kill_name(name).is_err(), "{name:?} must not be killable");
        }
        assert!(assert_kill_name("cdash-abc_1-2").is_ok());
    }

    #[test]
    fn assert_kill_name_admits_only_cdash_session_names() {
        // A2: this value is handed to `tmux kill-session -t`.
        assert!(assert_kill_name("cdash-backend-1531-a9f").is_ok());
        assert!(assert_kill_name("cdash-a_b-1").is_ok());
        assert!(assert_kill_name("other").is_err());
        assert!(assert_kill_name("cdash-").is_err(), "the suffix is required");
        assert!(assert_kill_name("cdash-x; rm -rf /").is_err());
        assert!(assert_kill_name("").is_err());
    }

    #[test]
    fn assert_valid_sid_admits_only_a_36_char_uuid_shape() {
        // A3: this value reaches `claude --resume <sid>` and a path join.
        assert!(assert_valid_sid("2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34").is_ok());
        assert!(assert_valid_sid("2F8A1C94-3B7E-4D51-9A02-6C5F8E1B7D34").is_ok(), "case-insensitive");
        assert!(assert_valid_sid("not-a-uuid; rm -rf /").is_err());
        assert!(assert_valid_sid("../../etc/passwd").is_err());
        assert!(assert_valid_sid("").is_err());
    }

    #[test]
    fn a_newline_cannot_smuggle_a_valid_line_past_the_anchors() {
        // JS `^…$` without the `m` flag behaves the same, but Rust's `regex`
        // makes it structural rather than a flag nobody set.
        assert!(assert_valid_sid("2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34\nrm -rf /").is_err());
        assert!(assert_kill_name("cdash-ok\nrm -rf /").is_err());
    }

    #[test]
    fn model_and_effort_are_allowlists_not_denylists() {
        assert!(assert_model("sonnet").is_ok());
        assert!(assert_model("gpt-4").is_err());
        assert!(assert_model("").is_err());
        assert!(assert_effort("xhigh").is_ok());
        assert!(assert_effort("ludicrous").is_err());
    }

    #[test]
    fn the_rejection_message_names_the_offending_value() {
        let e = assert_model("gpt-4").unwrap_err();
        assert!(e.0.contains("gpt-4"));
    }
}

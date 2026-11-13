//! Client frames and the text sanitizer.

use serde::Deserialize;

/// What a browser may send on `/api/chat`. Anything else fails to parse and is
/// ignored by the socket loop.
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ClientFrame {
    Send { cid: String, text: String },
    Stop,
    Held { op: HeldOp, id: String },
    Page { before: u64, limit: Option<usize> },
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HeldOp {
    Now,
    Remove,
}

/// Text that is about to be typed into a terminal. Line endings become `\n`,
/// every other control character (C0, DEL, C1) is dropped so an escape sequence
/// cannot end bracketed paste early, and the result is trimmed.
pub fn sanitize(text: &str) -> Result<String, &'static str> {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let kept: String = unified
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect();
    let out = kept.trim();
    if out.is_empty() {
        return Err("empty message");
    }
    if out.len() > super::MAX_TEXT {
        return Err("message too long");
    }
    Ok(out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_text_and_removes_everything_a_terminal_would_act_on() {
        assert_eq!(sanitize("a\tb\nc").unwrap(), "a\tb\nc");
        assert_eq!(sanitize("line1\r\nline2\rline3").unwrap(), "line1\nline2\nline3");
        // ESC [ 201 ~ ends bracketed paste; what follows would be typed as keys.
        assert_eq!(sanitize("x\x1b[201~rm -rf /\x07\x00\x7f\u{9b}y").unwrap(), "x[201~rm -rf /y");
        assert_eq!(sanitize("  /model opus  ").unwrap(), "/model opus");
    }
    #[test]
    fn sanitize_refuses_empty_and_oversized_text() {
        assert_eq!(sanitize(" \n\t "), Err("empty message"));
        assert_eq!(sanitize("\x1b\x1b"), Err("empty message"));
        assert_eq!(sanitize(&"a".repeat(crate::chat::MAX_TEXT + 1)), Err("message too long"));
        assert!(sanitize(&"a".repeat(crate::chat::MAX_TEXT)).is_ok());
    }
    #[test]
    fn client_frames_parse_and_unknown_ones_do_not() {
        let f = |s| serde_json::from_str::<ClientFrame>(s);
        assert!(matches!(f(r#"{"t":"send","cid":"c1","text":"hi"}"#), Ok(ClientFrame::Send { .. })));
        assert!(matches!(f(r#"{"t":"stop"}"#), Ok(ClientFrame::Stop)));
        assert!(matches!(f(r#"{"t":"held","op":"now","id":"h1"}"#), Ok(ClientFrame::Held { op: HeldOp::Now, .. })));
        assert!(matches!(f(r#"{"t":"page","before":40}"#), Ok(ClientFrame::Page { before: 40, limit: None })));
        assert!(f(r#"{"t":"keys","data":"x"}"#).is_err());
    }
}

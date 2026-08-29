use super::history::parse_lines;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transcript {
    pub branch: Option<String>,
    pub title: Option<String>,
    pub assistant_count: u32,
    pub last_assistant_text: Option<String>,
}

/// Parse a session transcript (newline-delimited JSON) into summary fields.
/// Mirrors `parseTranscript` in `lib/sessions.js:35-47`.
///
/// Fields are extracted leniently from each parsed line: a wrongly-typed
/// field is treated as absent for that field only, never as a reason to
/// drop the whole entry (only entries that are not valid JSON at all are
/// skipped, by `parse_lines`).
pub fn parse_transcript(jsonl: &str) -> Transcript {
    let mut t = Transcript::default();

    for e in parse_lines::<Value>(jsonl) {
        if t.branch.is_none() {
            if let Some(b) = e.get("gitBranch").and_then(|v| v.as_str()) {
                if !b.is_empty() && b != "HEAD" {
                    t.branch = Some(b.to_string());
                }
            }
        }

        if t.title.is_none() && e.get("type").and_then(|v| v.as_str()) == Some("ai-title") {
            if let Some(title) = e.get("aiTitle").and_then(|v| v.as_str()) {
                if !title.is_empty() {
                    t.title = Some(title.to_string());
                }
            }
        }

        if e.get("type").and_then(|v| v.as_str()) == Some("assistant") {
            t.assistant_count += 1;
            let text = e
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|items| {
                    items
                        .iter()
                        .find(|c| c.get("type").and_then(|v| v.as_str()) == Some("text"))
                })
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            // Node only assigns when a text part exists, so a tool-only
            // assistant turn leaves the previous value in place.
            if let Some(txt) = text {
                t.last_assistant_text = Some(txt.to_string());
            }
        }
    }

    t
}

/// Parse an `.rc` file's JSON body, extracting the bridge session id.
/// Mirrors `parseRcFile` in `lib/sessions.js:49-51`.
///
/// JS is `JSON.parse(jsonText).bridgeSessionId ?? null`: `??` only
/// substitutes for `null`/`undefined`, so any other JSON scalar (a number,
/// a bool) survives as a real value. That value later gets template-
/// interpolated into a URL by `rcLinkFor` (`lib/collect.js:87-91`), and JS
/// template interpolation stringifies whatever it's given. So we mirror
/// that stringification here rather than requiring a JSON string — do not
/// "simplify" this back to `.as_str()`, that would silently drop non-string
/// ids and break the "Open in Claude" link for them.
pub fn parse_rc_file(json: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(json).ok()?.get("bridgeSessionId")?.clone();
    match v {
        Value::Null => None,
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_branch_title_count_and_last_text() {
        let jsonl = [
            r#"{"type":"user","gitBranch":"main","message":{}}"#,
            r#"{"type":"ai-title","aiTitle":"Fix auth bug"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first reply"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use"},{"type":"text","text":"done, tests pass"}]}}"#,
        ]
        .join("\n");

        let t = parse_transcript(&jsonl);
        assert_eq!(t.branch.as_deref(), Some("main"));
        assert_eq!(t.title.as_deref(), Some("Fix auth bug"));
        assert_eq!(t.assistant_count, 2);
        assert_eq!(t.last_assistant_text.as_deref(), Some("done, tests pass"));
    }

    #[test]
    fn drops_head_branch_and_nulls_when_absent() {
        let t = parse_transcript(r#"{"type":"user","gitBranch":"HEAD"}"#);
        assert_eq!(t.branch, None);
        assert_eq!(t.title, None);
        assert_eq!(t.last_assistant_text, None);
    }

    #[test]
    fn assistant_without_text_content_does_not_clear_last_text() {
        let jsonl = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"kept"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use"}]}}"#,
        ]
        .join("\n");
        let t = parse_transcript(&jsonl);
        assert_eq!(t.assistant_count, 2);
        assert_eq!(t.last_assistant_text.as_deref(), Some("kept"));
    }

    #[test]
    fn rc_file_reads_bridge_session_id_or_none() {
        assert_eq!(
            parse_rc_file(r#"{"bridgeSessionId":"session_abc123"}"#).as_deref(),
            Some("session_abc123")
        );
        assert_eq!(parse_rc_file("garbage"), None);
        assert_eq!(parse_rc_file(r#"{"other":1}"#), None);
    }

    #[test]
    fn rc_file_bridge_session_id_type_variants() {
        assert_eq!(
            parse_rc_file(r#"{"bridgeSessionId":"abc"}"#).as_deref(),
            Some("abc")
        );
        assert_eq!(parse_rc_file(r#"{"bridgeSessionId":null}"#), None);
        assert_eq!(parse_rc_file(r#"{"other":1}"#), None);
        assert_eq!(
            parse_rc_file(r#"{"bridgeSessionId":123}"#).as_deref(),
            Some("123")
        );
        assert_eq!(parse_rc_file("not json"), None);
    }

    #[test]
    fn empty_string_branch_and_title_are_skipped_in_favor_of_later_entries() {
        let jsonl = [
            r#"{"type":"user","gitBranch":""}"#,
            r#"{"type":"ai-title","aiTitle":""}"#,
            r#"{"type":"user","gitBranch":"feature/x"}"#,
            r#"{"type":"ai-title","aiTitle":"Real Title"}"#,
        ]
        .join("\n");

        let t = parse_transcript(&jsonl);
        assert_eq!(t.branch.as_deref(), Some("feature/x"));
        assert_eq!(t.title.as_deref(), Some("Real Title"));
    }

    #[test]
    fn assistant_entry_with_non_object_message_or_non_array_content_still_counts() {
        let jsonl = [
            r#"{"type":"assistant","message":"not an object"}"#,
            r#"{"type":"assistant","message":{"content":"not an array"}}"#,
        ]
        .join("\n");

        let t = parse_transcript(&jsonl);
        assert_eq!(t.assistant_count, 2);
        assert_eq!(t.last_assistant_text, None);
    }

    #[test]
    fn assistant_text_item_with_non_string_value_is_not_captured_and_does_not_clear() {
        let jsonl = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"kept"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":42}]}}"#,
        ]
        .join("\n");

        let t = parse_transcript(&jsonl);
        assert_eq!(t.assistant_count, 2);
        assert_eq!(t.last_assistant_text.as_deref(), Some("kept"));
    }
}

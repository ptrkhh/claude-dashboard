//! Claude Code transcript lines to chat events. Pure: no I/O, never panics.
//!
//! The record shapes are internal to Claude Code (checked on 2.1.322, see
//! `CLAUDE_FINDINGS.md`). Anything not understood produces nothing.

use super::CAP;
use serde::Deserialize;
use serde_json::{json, Value};

/// Only the fields the rules read; serde skips the rest, so a multi-megabyte
/// `api-request-blob` costs a scan, not an allocation.
#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "type", default)]
    kind: String,
    subtype: Option<String>,
    uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    parent: Option<String>,
    #[serde(rename = "isSidechain", default)]
    sidechain: Option<bool>,
    #[serde(rename = "isMeta", default)]
    meta: Option<bool>,
    /// The summary written after compaction: context for the model, not a prompt.
    #[serde(rename = "isCompactSummary", default)]
    compact: Option<bool>,
    #[serde(rename = "isVisibleInTranscriptOnly", default)]
    transcript_only: Option<bool>,
    #[serde(rename = "promptSource")]
    source: Option<String>,
    operation: Option<String>,
    content: Option<Value>,
    attachment: Option<Value>,
    message: Option<Value>,
    #[serde(rename = "durationMs")]
    duration: Option<Value>,
}

/// The last prompt rule 7 emitted, for spotting a handed-back prompt.
struct Typed {
    queued: bool,
    parent: Option<String>,
    uuid: Option<String>,
}

#[derive(Default)]
pub struct Tap {
    open: bool,
    seen: usize,
    understood: bool,
    last: Option<Typed>,
}

impl Tap {
    /// True from a prompt until the next `result` event.
    pub fn turn_open(&self) -> bool {
        self.open
    }

    /// Five `user`/`assistant` records read and none produced an event: the
    /// format changed under us, and showing nothing is better than guessing.
    pub fn blind(&self) -> bool {
        self.seen >= 5 && !self.understood
    }

    pub fn feed(&mut self, line: &str) -> Vec<Value> {
        let Ok(r) = serde_json::from_str::<Rec>(line) else { return Vec::new() };
        if [r.sidechain, r.meta, r.compact, r.transcript_only].contains(&Some(true)) {
            return Vec::new();
        }
        let out = match r.kind.as_str() {
            "queue-operation" => match (r.operation.as_deref(), r.content.as_ref().and_then(Value::as_str)) {
                (Some("enqueue"), Some(text)) if !injected(text) => vec![json!({"type":"cdash","subtype":"queued","text":text})],
                _ => Vec::new(),
            },
            "attachment" => steer(&r).into_iter().collect(),
            "system" if r.subtype.as_deref() == Some("turn_duration") => {
                let mut ev = json!({"type":"result","subtype":"success"});
                if let Some(d) = r.duration.filter(Value::is_number) {
                    ev["duration_ms"] = d;
                }
                vec![ev]
            }
            "assistant" => assistant(&r),
            "user" => match r.message.as_ref().map(|m| &m["content"]) {
                Some(Value::Array(blocks)) => tool_results(&r, blocks),
                Some(Value::String(text)) => self.typed(&r, text),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        if r.kind == "user" || r.kind == "assistant" {
            self.seen += 1;
            self.understood |= !out.is_empty();
        }
        for ev in &out {
            match ev["type"].as_str() {
                Some("result") => self.open = false,
                Some("user") if ev["message"]["content"].is_string() => self.open = true,
                _ => {}
            }
        }
        out
    }

    /// Rule 7 and 8: a `user` record with string content.
    fn typed(&mut self, r: &Rec, text: &str) -> Vec<Value> {
        let t = text.trim_start();
        if let Some(cmd) = between(t, "<bash-input>", "</bash-input>") {
            return vec![json!({"type":"cdash","subtype":"shell","uuid":r.uuid,"command":cmd.trim()})];
        }
        if let Some(stdout) = between(t, "<bash-stdout>", "</bash-stdout>") {
            let stderr = between(t, "<bash-stderr>", "</bash-stderr>").unwrap_or("");
            return vec![json!({"type":"cdash","subtype":"shell_output","uuid":r.uuid,"stdout":stdout,"stderr":stderr})];
        }
        if r.source.as_deref() == Some("system") || injected(t) {
            return Vec::new();
        }
        let prompt = match between(t, "<command-name>", "</command-name>") {
            Some(name) => {
                let args = between(t, "<command-args>", "</command-args>").unwrap_or("");
                format!("{} {}", name.trim(), args.trim()).trim().to_string()
            }
            None => text.to_string(),
        };
        let queued = r.source.as_deref() == Some("queued");
        let mut out = Vec::new();
        if let Some(prev) = &self.last {
            // Claude hands a prompt stopped before any reply back to the input
            // box and writes the next one with the same parent.
            if !queued && !prev.queued && prev.parent.is_some() && prev.parent == r.parent {
                out.push(json!({"type":"cdash","subtype":"retract","uuid":prev.uuid}));
            }
        }
        self.last = Some(Typed { queued, parent: r.parent.clone(), uuid: r.uuid.clone() });
        out.push(prompt_event(r.uuid.as_deref(), &prompt, if queued { "queued" } else { "typed" }));
        out
    }
}

/// Text tools inject for the model's benefit; never shown as a user message.
fn injected(text: &str) -> bool {
    let t = text.trim_start();
    ["<task-notification>", "<system-reminder>", "<local-command-"].iter().any(|p| t.starts_with(p))
}

fn prompt_event(uuid: Option<&str>, text: &str, src: &str) -> Value {
    json!({"type":"user","uuid":uuid,"message":{"role":"user","content":text},"src":src})
}

/// Rule 3: a message Claude absorbed mid-turn has no `user` record, only this.
fn steer(r: &Rec) -> Option<Value> {
    let a = r.attachment.as_ref()?;
    if a["type"] != "queued_command" || a["commandMode"] != "prompt" {
        return None;
    }
    Some(prompt_event(r.uuid.as_deref(), a["prompt"].as_str()?, "steer"))
}

/// Rule 5: one event per `text` or `tool_use` block; thinking is status only.
fn assistant(r: &Rec) -> Vec<Value> {
    let Some(m) = &r.message else { return Vec::new() };
    let Some(blocks) = m["content"].as_array() else { return Vec::new() };
    blocks
        .iter()
        .filter_map(|b| {
            let block = match b["type"].as_str()? {
                "text" => json!({"type":"text","text":b["text"].as_str()?}),
                "tool_use" => {
                    let input = &b["input"];
                    let input = if input.to_string().len() > CAP { json!({"truncated":true}) } else { input.clone() };
                    json!({"type":"tool_use","id":b["id"],"name":b["name"],"input":input})
                }
                _ => return None,
            };
            Some(json!({"type":"assistant","uuid":r.uuid,"message":{"id":m["id"],"role":"assistant","content":[block]}}))
        })
        .collect()
}

/// Rule 6: tool results, and the marker Claude writes when a turn is stopped.
fn tool_results(r: &Rec, blocks: &[Value]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|b| match b["type"].as_str()? {
            "tool_result" => Some(json!({"type":"user","uuid":r.uuid,"message":{"role":"user","content":[{
                "type":"tool_result",
                "tool_use_id":b["tool_use_id"],
                "content":cap(&flatten(&b["content"])),
                "is_error":b["is_error"].as_bool().unwrap_or(false),
            }]}})),
            "text" if b["text"].as_str()?.starts_with("[Request interrupted by user") => {
                Some(json!({"type":"result","subtype":"interrupted"}))
            }
            _ => None,
        })
        .collect()
}

fn flatten(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn cap(s: &str) -> String {
    match s.char_indices().nth(CAP) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> (Vec<serde_json::Value>, Tap) {
        let mut tap = Tap::default();
        let evs = include_str!("testdata/claude.jsonl").lines().flat_map(|l| tap.feed(l)).collect();
        (evs, tap)
    }
    fn tag(e: &serde_json::Value) -> String {
        let c = &e["message"]["content"];
        match (e["type"].as_str().unwrap(), e["subtype"].as_str()) {
            ("user", _) if c.is_string() => format!("prompt:{}:{}", e["src"].as_str().unwrap(), c.as_str().unwrap()),
            ("user", _) => format!("tool_result:{}", c[0]["tool_use_id"].as_str().unwrap()),
            ("assistant", _) => format!("assistant:{}", c[0]["type"].as_str().unwrap()),
            (t, Some(s)) => format!("{t}:{s}"),
            (t, None) => t.to_string(),
        }
    }
    #[test]
    fn the_fixture_becomes_exactly_these_events_in_this_order() {
        let (evs, tap) = run();
        let tags: Vec<String> = evs.iter().map(tag).collect();
        assert_eq!(tags, [
            "prompt:typed:run the tests", "assistant:tool_use", "tool_result:toolu_1", "assistant:text", "result:success",
            "cdash:shell", "cdash:shell_output",
            "prompt:typed:slow job", "assistant:tool_use", "cdash:queued", "prompt:queued:also update README",
            "tool_result:toolu_2", "result:interrupted", "assistant:text", "result:success",
            "prompt:typed:handed back", "cdash:retract", "prompt:typed:second try",
            "assistant:tool_use", "cdash:queued", "tool_result:toolu_3", "prompt:steer:use -j4",
        ]);
        assert!(tap.turn_open(), "the steered message has not been answered yet");
    }
    #[test]
    fn details_chat_depends_on() {
        let (evs, _) = run();
        assert_eq!(evs[2]["message"]["content"][0]["content"], "12 passed", "block lists are flattened to text");
        assert_eq!(evs[4]["duration_ms"], 7773);
        assert_eq!(evs[5]["command"], "echo hi");
        assert_eq!((evs[6]["stdout"].as_str(), evs[6]["stderr"].as_str()), (Some("hi"), Some("")));
        assert_eq!(evs[9]["text"], "also update README");
        assert_eq!(evs[16]["uuid"], "u4", "the handed-back prompt is the one withdrawn");
        assert_eq!(evs[21]["uuid"], "q1", "an absorbed message takes the attachment's id");
        assert_eq!(evs[1]["message"]["id"], "msg_1");
    }
    #[test]
    fn oversized_and_unknown_lines_are_skipped_without_losing_what_follows() {
        let mut tap = Tap::default();
        let blob = format!(r#"{{"type":"api-request-blob","body":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
        assert!(tap.feed(&blob).is_empty());
        assert!(tap.feed(r#"{"type":"brand-new-record","uuid":"z"}"#).is_empty());
        let big = format!(r#"{{"type":"user","uuid":"r","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":"{}"}}]}}}}"#, "y".repeat(70_000));
        let ev = &tap.feed(&big)[0];
        assert_eq!(ev["message"]["content"][0]["content"].as_str().unwrap().chars().count(), crate::chat::CAP);
    }
    #[test]
    fn a_transcript_whose_shape_changed_is_reported_instead_of_shown_wrong() {
        let mut tap = Tap::default();
        for i in 0..5 { assert!(tap.feed(&format!(r#"{{"type":"assistant","uuid":"k{i}","payload":{{"parts":["new shape"]}}}}"#)).is_empty()); }
        assert!(tap.blind());
        let (_, healthy) = run();
        assert!(!healthy.blind());
    }
}

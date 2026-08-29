use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

const STOPWORDS: &[&str] = &[
    "continue", "resume", "exit", "usage", "ok", "yes", "no", "quit", "y", "n",
];

pub fn usable_prompts(displays: &[String]) -> Vec<String> {
    displays
        .iter()
        .filter(|d| {
            let t = d.trim();
            !t.is_empty()
                && !t.starts_with('/')
                && !STOPWORDS.contains(&t.to_lowercase().as_str())
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryGroup {
    pub sid: String,
    pub cwd: Option<String>,
    pub ts: f64,
    pub prompts: Vec<String>,
}

/// Parse newline-delimited JSON, silently skipping malformed lines.
/// Mirrors `parseLines` in `lib/sessions.js:10-17`.
pub(crate) fn parse_lines<T: for<'de> Deserialize<'de>>(text: &str) -> Vec<T> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<T>(l).ok())
        .collect()
}

pub fn group_history(jsonl: &str) -> Vec<HistoryGroup> {
    struct Acc {
        sid: String,
        cwd: Option<String>,
        ts: f64,
        displays: Vec<String>,
    }

    let mut by_sid: HashMap<String, Acc> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for e in parse_lines::<Value>(jsonl) {
        // Mirrors Node's `if (!e.sessionId) continue` — missing, null, and
        // empty-string session ids are all treated as falsy and skipped.
        let sid = match e.get("sessionId").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        // Wrong-typed fields are treated as absent for that field only —
        // never a reason to drop the whole line (unlike a strict struct
        // deserialize, which would fail the entry entirely).
        let project = e.get("project").and_then(|v| v.as_str()).map(|s| s.to_string());
        // Extract timestamp leniently: accept JSON numbers (both int and float),
        // or strings that parse as f64. Anything else contributes 0.0.
        let timestamp = e.get("timestamp")
            .and_then(|v| v.as_f64())
            .or_else(|| e.get("timestamp").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()))
            .unwrap_or(0.0);
        let display = e.get("display").and_then(|v| v.as_str()).map(|s| s.to_string());

        let acc = by_sid.entry(sid.clone()).or_insert_with(|| {
            order.push(sid.clone());
            Acc { sid: sid.clone(), cwd: None, ts: 0.0, displays: Vec::new() }
        });
        // Node: `g.cwd = e.project ?? g.cwd` — only replaces when present.
        // Note "" is a valid project/cwd value, distinct from absent.
        if project.is_some() {
            acc.cwd = project;
        }
        acc.ts = f64::max(acc.ts, timestamp);
        if let Some(d) = display {
            acc.displays.push(d);
        }
    }

    let mut groups: Vec<HistoryGroup> = order
        .into_iter()
        .filter_map(|sid| by_sid.remove(&sid))
        .map(|a| {
            let usable = usable_prompts(&a.displays);
            let start = usable.len().saturating_sub(3);
            HistoryGroup { sid: a.sid, cwd: a.cwd, ts: a.ts, prompts: usable[start..].to_vec() }
        })
        .collect();

    groups.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
    groups.truncate(60);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn usable_prompts_filters_junk() {
        let input = s(&["/model", "", "ok", "YES", "fix the auth bug", "continue", "add tests"]);
        assert_eq!(usable_prompts(&input), s(&["fix the auth bug", "add tests"]));
    }

    #[test]
    fn group_history_groups_sorts_and_keeps_last_three() {
        let jsonl = [
            r#"{"sessionId":"a","project":"/x","timestamp":100,"display":"first prompt"}"#,
            r#"{"sessionId":"b","project":"/y","timestamp":300,"display":"other session"}"#,
            r#"{"sessionId":"a","project":"/x","timestamp":200,"display":"p2"}"#,
            r#"{"sessionId":"a","project":"/x","timestamp":250,"display":"p3"}"#,
            r#"{"sessionId":"a","project":"/x","timestamp":260,"display":"p4"}"#,
            "not json — must be skipped",
        ]
        .join("\n");

        let g = group_history(&jsonl);
        assert_eq!(g[0].sid, "b");
        assert_eq!(g[1].sid, "a");
        assert_eq!(g[1].ts, 260.0);
        assert_eq!(g[1].cwd.as_deref(), Some("/x"));
        assert_eq!(g[1].prompts, s(&["p2", "p3", "p4"]));
    }

    #[test]
    fn group_history_skips_entries_without_a_session_id() {
        let jsonl = r#"{"project":"/x","timestamp":100,"display":"orphan"}"#;
        assert!(group_history(jsonl).is_empty());
    }

    #[test]
    fn group_history_tolerates_wrongly_typed_display() {
        // display:5 is not a string; the entry must still be grouped, and
        // cwd/ts still applied — only the display itself is dropped.
        let jsonl = r#"{"sessionId":"a","project":"/x","timestamp":9,"display":5}"#;
        let g = group_history(jsonl);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].sid, "a");
        assert_eq!(g[0].cwd.as_deref(), Some("/x"));
        assert_eq!(g[0].ts, 9.0);
        assert!(g[0].prompts.is_empty());
    }

    #[test]
    fn group_history_skips_empty_string_session_id() {
        let jsonl = r#"{"sessionId":"","project":"/x","timestamp":9.0,"display":"hi"}"#;
        assert!(group_history(jsonl).is_empty());
    }

    #[test]
    fn group_history_keeps_empty_string_project_as_cwd() {
        // "" is a legitimate cwd value, distinct from an absent project.
        let jsonl = r#"{"sessionId":"a","project":"","timestamp":9.0,"display":"hi"}"#;
        let g = group_history(jsonl);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].cwd.as_deref(), Some(""));
    }

    #[test]
    fn group_history_stable_sorts_equal_timestamps() {
        let jsonl = [
            r#"{"sessionId":"first","project":"/x","timestamp":5.0,"display":"a"}"#,
            r#"{"sessionId":"second","project":"/y","timestamp":5.0,"display":"b"}"#,
        ]
        .join("\n");

        let g = group_history(&jsonl);
        assert_eq!(g[0].sid, "first");
        assert_eq!(g[1].sid, "second");
    }

    #[test]
    fn group_history_accepts_float_timestamp() {
        // JSON floats like 100.5 must be honored as 100.5, not truncated to 0.
        let jsonl = r#"{"sessionId":"a","project":"/x","timestamp":100.5,"display":"test"}"#;
        let g = group_history(jsonl);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ts, 100.5);
    }

    #[test]
    fn group_history_accepts_numeric_string_timestamp() {
        // Numeric strings like "12345" must be parsed and honored as 12345.0.
        let jsonl = r#"{"sessionId":"a","project":"/x","timestamp":"12345","display":"test"}"#;
        let g = group_history(jsonl);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ts, 12345.0);
    }
}

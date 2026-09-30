//! Typing into a running session's TUI from the dashboard.
//!
//! The Claude app's remote control can send prompts but can't type into the TUI
//! itself, so when a session stops and asks you to run something ("please run
//! `! gcloud auth login`") there's no way to answer it from a phone. This sends
//! the keystrokes straight to the tmux pane instead. Stopgap — delete the whole
//! feature once remote control grows an input.
//!
//! Port of `lib/keys.js` and the `POST /api/keys` handler in `server.js`.

use super::ctx::Ctx;
use super::validate::{assert_kill_name, BadRequest, Refused};
use std::sync::Arc;
use std::time::Duration;

/// Mirrors `MAX_TEXT` (`lib/keys.js:12`). Counted in `char`s, where Node
/// counted UTF-16 units; the cap is a sanity bound, not a security boundary,
/// and the two agree for everything below the astral planes.
pub const MAX_TEXT: usize = 4096;

/// Claude's TUI coalesces a fast burst into a paste; a beat's pause keeps the
/// Enter a keypress that submits rather than part of the pasted text.
pub const ENTER_DELAY: Duration = Duration::from_millis(50);

/// A validated send-keys request.
#[derive(Debug, Clone, PartialEq)]
pub struct SendKeys {
    pub name: String,
    pub text: String,
}

/// C0 controls minus tab and the newlines [`parse_send_keys`] collapses, plus
/// DEL. Stripped so a paste can't smuggle escape sequences (arrow keys, mode
/// switches) into the TUI — everything that arrives is literal text.
///
/// Mirrors `CONTROL_RE` (`lib/keys.js:16`) exactly: `\t` is *not* in the class.
fn is_stripped_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}

/// Validate and normalize a send-keys request body.
///
/// `text` is `Option` rather than a defaulted `String` so an absent field is
/// "text required" and a whitespace-only one is "empty text", as Node's
/// `typeof raw !== 'string'` branch distinguished them.
pub fn parse_send_keys(name: &str, text: Option<&str>) -> Result<SendKeys, BadRequest> {
    // The same guard `/api/kill` uses: only panes this dashboard named. Both
    // values reach `tmux -t`, so they answer to one allowlist.
    assert_kill_name(name)?;

    let raw = text.ok_or_else(|| BadRequest("text required".to_string()))?;
    if raw.chars().count() > MAX_TEXT {
        return Err(BadRequest("text too long".to_string()));
    }

    // The TUI submits at the first newline, so a multi-line paste would fire
    // the first line and scatter the rest into whatever prompt came next.
    // Collapse each *run* of newlines to one space and let the caller append a
    // single Enter.
    let mut out = String::with_capacity(raw.len());
    let mut in_newline_run = false;
    for c in raw.chars() {
        if c == '\r' || c == '\n' {
            if !in_newline_run {
                out.push(' ');
                in_newline_run = true;
            }
            continue;
        }
        in_newline_run = false;
        if !is_stripped_control(c) {
            out.push(c);
        }
    }

    let text = out.trim().to_string();
    if text.is_empty() {
        return Err(BadRequest("empty text".to_string()));
    }
    Ok(SendKeys { name: name.to_string(), text })
}

/// tmux argv for typing `text` into the pane — one or more commands — and then
/// the one that submits it.
///
/// Enter is separate because `-l` sends its operand literally: "Enter" typed
/// literally is the five characters, not the key. The `--` is load-bearing —
/// without it tmux reads a leading-dash command (`--version`) as its own flag
/// and errors.
///
/// Two more things tmux does that "literal" does not cover:
/// - A `;` ending any argument is a command separator, even after `-l`, and it
///   eats a `\` in front of it: `echo hi;` arrives as `echo hi`, and
///   `find . -exec rm {} \;` as `find . -exec rm {} ;`. Trailing semicolons are
///   split off and sent as hex keys (`-H 3b`) instead. Only the end of the
///   argument matters; a `;` anywhere else in the text goes through as typed.
/// - `-t name` falls back to prefix matching, so when the session has just died
///   the keystrokes land in a sibling (`…-k2p` becomes `…-k2pX`). `=name:` is an
///   exact match and fails instead.
pub fn send_keys_args(k: &SendKeys) -> (Vec<Vec<String>>, Vec<String>) {
    let target = format!("={}:", k.name);
    let cmd = |tail: &[&str]| -> Vec<String> {
        ["send-keys", "-t", &target].iter().chain(tail).map(|a| a.to_string()).collect()
    };

    let body = k.text.trim_end_matches(';');
    let semicolons = k.text.len() - body.len(); // `;` is one byte
    let mut typed = Vec::new();
    if !body.is_empty() {
        typed.push(cmd(&["-l", "--", body]));
    }
    if semicolons > 0 {
        let mut hex = vec!["-H"];
        hex.extend(std::iter::repeat_n("3b", semicolons));
        typed.push(cmd(&hex));
    }
    (typed, cmd(&["Enter"]))
}

async fn tmux(ctx: &Arc<Ctx>, args: &[String], key: &str) -> Result<(), Refused> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    ctx.runner.run_checked("tmux", &args, key).await.map(drop).map_err(Refused::Failed)
}

/// Type into a session's TUI, then submit.
///
/// `run_checked` on both halves, as every mutating route does: reporting a
/// keystroke that never reached the pane is worse than reporting the error.
pub async fn send_keys(ctx: &Arc<Ctx>, name: &str, text: Option<&str>) -> Result<(), Refused> {
    let keys = parse_send_keys(name, text)?;
    let (typed, enter) = send_keys_args(&keys);

    for args in &typed {
        tmux(ctx, args, "tmux send-keys").await?;
    }
    tokio::time::sleep(ENTER_DELAY).await;
    tmux(ctx, &enter, "tmux send-keys Enter").await?;

    ctx.host.log.push(log_line(&keys));
    Ok(())
}

/// What the log panel gets. Never the text: this bar exists to answer prompts,
/// and what people type at them is a login code or a token, which `/api/logs`
/// would then serve to anyone who can open the dashboard.
fn log_line(k: &SendKeys) -> String {
    format!("keys {}: {} chars", k.name, k.text.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(name: &str, text: &str) -> Result<SendKeys, BadRequest> {
        parse_send_keys(name, Some(text))
    }

    #[test]
    fn accepts_a_cdash_session_name_and_trims_the_text() {
        assert_eq!(
            parse("cdash-infra-0847-k2p", "  ! gcloud auth login  ").unwrap(),
            SendKeys {
                name: "cdash-infra-0847-k2p".to_string(),
                text: "! gcloud auth login".to_string(),
            }
        );
    }

    #[test]
    fn rejects_names_outside_the_cdash_namespace() {
        // A2 again: this value reaches `tmux send-keys -t`.
        for name in ["other-session", "cdash-a; rm -rf /", "cdash-a b", "", "cdash-"] {
            assert!(parse(name, "hi").is_err(), "{name:?} must not be addressable");
        }
    }

    #[test]
    fn rejects_missing_empty_and_whitespace_only_text() {
        assert_eq!(
            parse_send_keys("cdash-a", None).unwrap_err(),
            BadRequest("text required".to_string()),
            "an absent field must not become an empty keystroke"
        );
        assert!(parse("cdash-a", "   ").unwrap_err().0.contains("empty text"));
        assert!(parse("cdash-a", "\n\n").unwrap_err().0.contains("empty text"));
    }

    #[test]
    fn rejects_text_past_the_length_cap() {
        let long = "x".repeat(MAX_TEXT + 1);
        assert!(parse("cdash-a", &long).unwrap_err().0.contains("too long"));
        assert!(parse("cdash-a", &"x".repeat(MAX_TEXT)).is_ok());
    }

    #[test]
    fn collapses_newline_runs_so_a_paste_submits_once() {
        // A multi-line paste would otherwise fire line 1 and leave the rest to
        // land in whatever prompt came next. `\r\n` is one run, not two.
        assert_eq!(parse("cdash-a", "one\ntwo\r\nthree").unwrap().text, "one two three");
    }

    #[test]
    fn strips_control_characters_that_would_act_as_keys_in_the_tui() {
        assert_eq!(parse("cdash-a", "safe\u{1b}[Atext\u{0}").unwrap().text, "safe[Atext");
    }

    #[test]
    fn keeps_tab_which_the_node_class_also_spared() {
        assert_eq!(parse("cdash-a", "a\tb").unwrap().text, "a\tb");
    }

    #[test]
    fn the_log_line_says_how_much_was_sent_not_what() {
        let k = SendKeys { name: "cdash-a".to_string(), text: "sk-ant-oat01-secret".to_string() };
        assert_eq!(log_line(&k), "keys cdash-a: 19 chars");
    }

    fn args(text: &str) -> (Vec<Vec<String>>, Vec<String>) {
        send_keys_args(&SendKeys { name: "cdash-a".to_string(), text: text.to_string() })
    }

    #[test]
    fn send_keys_args_passes_text_as_a_literal_operand_after_the_separator() {
        // Without `--`, tmux reads a leading-dash command as its own flag.
        let (typed, enter) = args("--version");
        assert_eq!(typed, [["send-keys", "-t", "=cdash-a:", "-l", "--", "--version"]]);
        assert_eq!(enter, ["send-keys", "-t", "=cdash-a:", "Enter"]);
    }

    #[test]
    fn the_target_is_an_exact_session_match_not_a_prefix() {
        // Bare `cdash-a` would type into `cdash-ab` once `cdash-a` is gone.
        let (typed, enter) = args("hi");
        assert!(typed.iter().chain([&enter]).all(|c| c[2] == "=cdash-a:"));
    }

    #[test]
    fn trailing_semicolons_go_as_hex_keys_because_tmux_eats_them_as_separators() {
        let hex = |n: usize| {
            ["send-keys", "-t", "=cdash-a:", "-H"].into_iter().chain(vec!["3b"; n]).collect::<Vec<_>>()
        };
        // `echo hi;` -> "echo hi" then one `;`
        let (typed, _) = args("echo hi;");
        assert_eq!(typed, [vec!["send-keys", "-t", "=cdash-a:", "-l", "--", "echo hi"], hex(1)]);
        // A backslash before the `;` is ordinary text once the `;` is not tmux's to parse.
        let (typed, _) = args(r"find . -exec rm {} \;");
        assert_eq!(typed[0][5], r"find . -exec rm {} \");
        assert_eq!(typed[1], hex(1));
        // Nothing but semicolons: no empty literal, one hex command.
        assert_eq!(args(";;").0, [hex(2)]);
        // A `;` that does not end the text is not special and is sent as typed.
        assert_eq!(args("a;b ; c").0, [vec!["send-keys", "-t", "=cdash-a:", "-l", "--", "a;b ; c"]]);
    }
}

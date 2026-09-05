//! Task Scheduler registration for the scheduled binary. `task_xml` is pure
//! and tested everywhere; `install` and `uninstall` drive `schtasks` on
//! Windows only.

#[cfg(windows)]
use super::cmd::Runner;
#[cfg(windows)]
use std::time::Duration;

pub const TASK_NAME: &str = "cdash-agent";
/// The windowless twin the task runs; it must sit beside `cdash-agent.exe`.
pub const SCHEDULED_EXE: &str = "cdash-agentw.exe";
#[cfg(windows)]
const SCHTASKS_TIMEOUT: Duration = Duration::from_secs(30);

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The task definition. Element order follows what `schtasks /Query /XML`
/// exports, because the schema is a sequence. The settings that matter:
/// `PT0S` (the default PT72H kills the agent after three days), `IgnoreNew`
/// (a repetition tick or a second logon while the agent lives is a no-op),
/// the five-minute repetition (the only restart the scheduler offers —
/// `RestartOnFailure` counts only an action it could not start), and
/// priority 4 (the default 7 is BELOW_NORMAL with low I/O and memory
/// priority, inherited by every `claude` the agent spawns).
pub fn task_xml(exe: &str, working_dir: &str, user: &str) -> String {
    let (exe, dir, user) = (xml_escape(exe), xml_escape(working_dir), xml_escape(user));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Repetition>
        <Interval>PT5M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// `DOMAIN\name` for the trigger and the principal. Refused rather than
/// defaulted: a native `install` launched from a WSL shell inherits WSL's
/// environment, where neither variable exists, and the empty halves would
/// register the task against a principal that never logs on — an install
/// that reports success and an agent that never starts.
pub fn task_user(domain: &str, name: &str) -> Result<String, String> {
    if domain.is_empty() || name.is_empty() {
        return Err("USERDOMAIN and USERNAME must both be set; run install from a \
                    Windows console, not a WSL shell"
            .to_string());
    }
    Ok(format!("{domain}\\{name}"))
}

/// What `schtasks /Query /XML` exports: UTF-16LE with a byte-order mark.
pub fn utf16le_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Register the task for the current user and start it. Order matters: an
/// earlier instance is ended first, or `IgnoreNew` makes `/Run` a silent
/// no-op and an upgrade keeps running the old binary. Re-running `install`
/// is also how `setx` changes are applied. On a first install the `/End`
/// line fails and is ignored; its log echo on stderr is expected.
#[cfg(windows)]
pub async fn install(runner: &Runner) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let dir = exe.parent().ok_or("the executable has no parent directory")?.to_path_buf();
    let agentw = dir.join(SCHEDULED_EXE);
    if !agentw.is_file() {
        return Err(format!(
            "{} not found beside {}; the scheduled task runs the windowless binary",
            agentw.display(),
            exe.display()
        ));
    }
    let user = task_user(
        &std::env::var("USERDOMAIN").unwrap_or_default(),
        &std::env::var("USERNAME").unwrap_or_default(),
    )?;
    let xml = task_xml(&agentw.to_string_lossy(), &dir.to_string_lossy(), &user);
    let tmp = std::env::temp_dir().join("cdash-agent-task.xml");
    std::fs::write(&tmp, utf16le_bom(&xml)).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    let tmp_s = tmp.to_string_lossy().into_owned();

    let _ = runner
        .run_checked_with_timeout("schtasks", &["/End", "/TN", TASK_NAME], "schtasks end", SCHTASKS_TIMEOUT)
        .await;
    let created = runner
        .run_checked_with_timeout(
            "schtasks",
            &["/Create", "/TN", TASK_NAME, "/XML", &tmp_s, "/F"],
            "schtasks create",
            SCHTASKS_TIMEOUT,
        )
        .await;
    let _ = std::fs::remove_file(&tmp);
    created?;
    runner
        .run_checked_with_timeout("schtasks", &["/Run", "/TN", TASK_NAME], "schtasks run", SCHTASKS_TIMEOUT)
        .await?;

    // The scheduled instance's exit status is invisible to anyone; the URL is
    // the check.
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    Ok(format!(
        "registered task {TASK_NAME}: {} at logon for {user}, retried every 5 minutes while stopped\nopen http://127.0.0.1:{port}",
        agentw.display()
    ))
}

#[cfg(windows)]
pub async fn uninstall(runner: &Runner) -> Result<String, String> {
    let _ = runner
        .run_checked_with_timeout("schtasks", &["/End", "/TN", TASK_NAME], "schtasks end", SCHTASKS_TIMEOUT)
        .await;
    runner
        .run_checked_with_timeout(
            "schtasks",
            &["/Delete", "/TN", TASK_NAME, "/F"],
            "schtasks delete",
            SCHTASKS_TIMEOUT,
        )
        .await?;
    Ok(format!("removed task {TASK_NAME}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_has_every_setting_the_design_depends_on() {
        let xml = task_xml(r"C:\cdash\cdash-agentw.exe", r"C:\cdash", r"PC\pat");
        // The default PT72H kills the agent after three days.
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"), "{xml}");
        // A repetition tick while the agent lives must be a no-op.
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        // The only restart the scheduler offers: RestartOnFailure never fires on an exit.
        assert!(xml.contains("<Interval>PT5M</Interval>"));
        assert!(!xml.contains("RestartOnFailure"));
        // The user's desktop session: WSL, the share, console windows.
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<LogonTrigger>"));
        // The default 7 is BELOW_NORMAL, inherited by every claude the agent spawns.
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(xml.contains(r"<Command>C:\cdash\cdash-agentw.exe</Command>"));
        assert!(xml.contains(r"<WorkingDirectory>C:\cdash</WorkingDirectory>"));
        assert_eq!(xml.matches(r"<UserId>PC\pat</UserId>").count(), 2, "trigger and principal");
        assert!(xml.starts_with(r#"<?xml version="1.0" encoding="UTF-16"?>"#));
    }

    #[test]
    fn xml_special_characters_in_paths_are_escaped() {
        let xml = task_xml(r"C:\a & b\cdash-agentw.exe", r"C:\a & b", "u");
        assert!(xml.contains(r"C:\a &amp; b\cdash-agentw.exe"));
        assert!(!xml.contains("a & b"));

        // Every interpolation lands in element text, so & < > " are escaped
        // wherever they reach and ' needs no escape there.
        let xml = task_xml("e<x>e", r#"d"ir"#, r"PC\o'e");
        assert!(xml.contains("<Command>e&lt;x&gt;e</Command>"), "{xml}");
        assert!(xml.contains("<WorkingDirectory>d&quot;ir</WorkingDirectory>"), "{xml}");
        assert!(xml.contains(r"<UserId>PC\o'e</UserId>"), "{xml}");
    }

    #[test]
    fn a_user_missing_either_half_is_refused_before_any_xml_is_written() {
        assert_eq!(task_user("PC", "pat").unwrap(), r"PC\pat");
        // A native install run from a WSL shell inherits WSL's environment,
        // where neither variable exists; `\` would register the task against
        // a principal that never logs on.
        for (domain, name) in [("", ""), ("", "pat"), ("PC", "")] {
            let e = task_user(domain, name).unwrap_err();
            assert!(e.contains("USERDOMAIN"), "{domain:?}/{name:?}: {e}");
        }
    }

    #[test]
    fn the_file_is_utf16le_with_a_bom() {
        let bytes = utf16le_bom("<T/>");
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        let units: Vec<u16> = bytes[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "<T/>");
    }
}

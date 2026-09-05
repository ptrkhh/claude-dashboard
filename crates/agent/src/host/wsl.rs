//! The WSL bridge's pure parts: how the distro is reached, how its paths map
//! onto the `\\wsl.localhost` share, and how its process list is read. All of
//! it compiles and is tested on every host; only `probe_wsl` is Windows.

use crate::host::proc::ProcRow;

/// Run once at boot inside the distro with `sh -lc`: the login-shell PATH
/// then the home directory as the share sees it (`wslpath -w`). It is the
/// last thing the login shell prints, which is how the parser finds it.
pub const PROBE_SCRIPT: &str = r#"printf "%s\n%s\n" "$PATH" "$(wslpath -w "$HOME")""#;

/// One line per binary the WSL side needs and lacks. Re-run per
/// `/api/hostinfo` call, like the native list.
pub const MISSING_SCRIPT: &str =
    r#"for b in tmux claude git; do command -v "$b" >/dev/null 2>&1 || printf "%s\n" "$b"; done"#;

/// `ps` columns, `comm` last so a space in a command name cannot shift the
/// numeric fields.
pub const PS_ARGS: &[&str] = &["-eo", "pid=,ppid=,%cpu=,rss=,comm="];

#[derive(Debug, Clone, PartialEq)]
pub struct WslProbe {
    /// The login-shell PATH inside the distro.
    pub path: String,
    /// `\\wsl.localhost\Ubuntu\home\u` or, on older WSL, `\\wsl$\Ubuntu\home\u`.
    pub home_unc: String,
    /// `CDASH_WSL_DISTRO` when set. `None` means the default distro: no `-d`.
    pub distro_flag: Option<String>,
}

pub fn parse_wsl_probe(out: &str) -> Option<WslProbe> {
    // The LAST two lines, not the first: `sh -lc` sources /etc/profile and
    // ~/.profile, and a MOTD or a version-manager banner there prints before
    // the script does. The script's own printf is always last.
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty()).rev();
    let home_unc = lines.next()?.to_string();
    let path = lines.next()?.to_string();
    if !home_unc.starts_with("\\\\") {
        return None;
    }
    Some(WslProbe { path, home_unc, distro_flag: None })
}

/// The command prefix that turns a native `Runner` into the WSL side's:
/// `--exec` skips the distro's shell so arguments arrive unchanged, and `env`
/// applies the probed login PATH and a C locale without sourcing a profile
/// per call.
pub fn wsl_prefix(distro_flag: Option<&str>, path: &str) -> Vec<String> {
    let mut v = vec!["wsl.exe".to_string()];
    if let Some(d) = distro_flag {
        v.push("-d".to_string());
        v.push(d.to_string());
    }
    v.push("--exec".to_string());
    v.push("/usr/bin/env".to_string());
    v.push(format!("PATH={path}"));
    // `ps` prints %cpu through the C library's locale; under a comma-decimal
    // one every row would fail to parse and the alive set would empty.
    v.push("LC_ALL=C".to_string());
    v
}

/// Both spellings of the share host; the share itself is case-insensitive.
const SHARE_HOSTS: &[&str] = &["wsl.localhost", "wsl$"];

#[derive(Debug, Clone, PartialEq)]
pub struct WslPaths {
    /// `\\wsl.localhost\Ubuntu`, no trailing separator.
    pub unc_root: String,
    pub distro: String,
}

impl WslPaths {
    /// From the probe's home line.
    pub fn from_home_unc(home_unc: &str) -> Option<Self> {
        let rest = home_unc.strip_prefix("\\\\")?;
        let mut parts = rest.split('\\');
        let host = parts.next()?;
        let distro = parts.next()?;
        if distro.is_empty() || !SHARE_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return None;
        }
        Some(Self { unc_root: format!("\\\\{host}\\{distro}"), distro: distro.to_string() })
    }

    pub fn to_unc(&self, linux: &str) -> String {
        format!("{}{}", self.unc_root, linux.replace('/', "\\"))
    }

    /// `\\wsl.localhost\<distro>\a\b` or `\\wsl$\<distro>\a\b` → `/a/b`, for
    /// this distro only. The bare root maps to `/`. Anything else is `None`,
    /// which the router turns into a 400 rather than a launch elsewhere.
    pub fn from_unc(&self, unc: &str) -> Option<String> {
        let rest = unc.strip_prefix("\\\\")?;
        let (host, after_host) = rest.split_once('\\')?;
        if !SHARE_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return None;
        }
        let (distro, tail) = match after_host.split_once('\\') {
            Some((d, t)) => (d, t),
            None => (after_host, ""),
        };
        if !distro.eq_ignore_ascii_case(&self.distro) {
            return None;
        }
        let linux = format!("/{}", tail.replace('\\', "/"));
        Some(if linux.len() > 1 { linux.trim_end_matches('/').to_string() } else { linux })
    }
}

/// `ps -eo pid=,ppid=,%cpu=,rss=,comm=` → rows. `%cpu` is the process's
/// lifetime average, which is what the Node agent showed. Lines that do not
/// parse are skipped.
pub fn parse_ps(out: &str) -> Vec<ProcRow> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse::<i32>().ok()?;
            let ppid = it.next()?.parse::<i32>().ok()?;
            let cpu = it.next()?.parse::<f32>().ok()?;
            let rss_kb = it.next()?.parse::<u64>().ok()?;
            let name = it.collect::<Vec<_>>().join(" ");
            if name.is_empty() {
                return None;
            }
            Some(ProcRow { pid, ppid, name, cpu, rss_kb })
        })
        .collect()
}

/// The first `wsl.exe` call may cold-start the distro; 5 seconds is not
/// enough for that and 30 is.
#[cfg(windows)]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Ask the distro for its login PATH and its home as the share sees it.
/// `None` — with the reason logged — means the agent runs with the native
/// side alone: `wsl.exe` absent, the distro failing, a timeout, or output
/// that is not the two lines `PROBE_SCRIPT` prints.
#[cfg(windows)]
pub async fn probe_wsl(
    native: &crate::host::cmd::Runner,
    log: &crate::host::log::LogBuffer,
) -> Option<WslProbe> {
    if std::env::var("CDASH_WSL").as_deref() == Ok("0") {
        log.push("wsl: disabled by CDASH_WSL=0; Windows side only");
        return None;
    }
    let distro_flag = std::env::var("CDASH_WSL_DISTRO").ok().filter(|s| !s.is_empty());
    let mut args: Vec<&str> = Vec::new();
    if let Some(d) = distro_flag.as_deref() {
        args.push("-d");
        args.push(d);
    }
    args.extend_from_slice(&["--exec", "/bin/sh", "-lc", PROBE_SCRIPT]);

    // One log line for a failed probe, not two: the `Err` arm below says the
    // same thing and adds the consequence.
    native.silence("wsl probe");
    match native.run_checked_with_timeout("wsl.exe", &args, "wsl probe", PROBE_TIMEOUT).await {
        Ok(out) => match parse_wsl_probe(&out) {
            Some(mut p) => {
                p.distro_flag = distro_flag;
                Some(p)
            }
            None => {
                log.push(format!(
                    "wsl: unexpected probe output {:?}; Windows side only",
                    out.trim()
                ));
                None
            }
        },
        Err(e) => {
            log.push(format!("wsl: {e}; Windows side only"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_output_is_path_then_home() {
        let p = parse_wsl_probe("/home/u/.local/bin:/usr/bin\n\\\\wsl.localhost\\Ubuntu\\home\\u\n").unwrap();
        assert_eq!(p.path, "/home/u/.local/bin:/usr/bin");
        assert_eq!(p.home_unc, "\\\\wsl.localhost\\Ubuntu\\home\\u");
        assert_eq!(p.distro_flag, None);
    }

    #[test]
    fn profile_noise_before_the_two_lines_is_ignored() {
        // `sh -lc` sources /etc/profile and ~/.profile; a MOTD or an nvm
        // banner there used to shift the two real lines out of reach and drop
        // the WSL side for the whole session.
        let p = parse_wsl_probe(
            "Welcome to Ubuntu 24.04 LTS\n\nnvm: using node v22\n/usr/bin\n\\\\wsl.localhost\\Ubuntu\\home\\u\n",
        )
        .unwrap();
        assert_eq!(p.path, "/usr/bin");
        assert_eq!(p.home_unc, "\\\\wsl.localhost\\Ubuntu\\home\\u");
    }

    #[test]
    fn probe_output_without_a_unc_home_is_rejected() {
        // A distro whose wslpath is broken prints something else as its last
        // line; building a share path from it would read the wrong disk.
        assert!(parse_wsl_probe("/usr/bin\n/home/u\n").is_none());
        assert!(parse_wsl_probe("").is_none());
        assert!(parse_wsl_probe("/usr/bin\n").is_none());
    }

    #[test]
    fn the_prefix_names_the_distro_only_when_asked() {
        assert_eq!(
            wsl_prefix(None, "/usr/bin"),
            vec!["wsl.exe", "--exec", "/usr/bin/env", "PATH=/usr/bin", "LC_ALL=C"]
        );
        assert_eq!(
            wsl_prefix(Some("Debian"), "/usr/bin"),
            vec!["wsl.exe", "-d", "Debian", "--exec", "/usr/bin/env", "PATH=/usr/bin", "LC_ALL=C"]
        );
    }

    #[test]
    fn paths_come_from_the_home_line_under_either_share_host() {
        let new = WslPaths::from_home_unc("\\\\wsl.localhost\\Ubuntu\\home\\u").unwrap();
        assert_eq!(new.unc_root, "\\\\wsl.localhost\\Ubuntu");
        assert_eq!(new.distro, "Ubuntu");
        let old = WslPaths::from_home_unc("\\\\wsl$\\Ubuntu-22.04\\root").unwrap();
        assert_eq!(old.unc_root, "\\\\wsl$\\Ubuntu-22.04");
        assert_eq!(old.distro, "Ubuntu-22.04");
        assert!(WslPaths::from_home_unc("\\\\server\\share\\x").is_none());
        assert!(WslPaths::from_home_unc("C:\\Users\\u").is_none());
    }

    fn ubuntu() -> WslPaths {
        WslPaths { unc_root: "\\\\wsl.localhost\\Ubuntu".into(), distro: "Ubuntu".into() }
    }

    #[test]
    fn to_unc_maps_a_linux_path_onto_the_share() {
        assert_eq!(ubuntu().to_unc("/home/u/p"), "\\\\wsl.localhost\\Ubuntu\\home\\u\\p");
        assert_eq!(ubuntu().to_unc("/"), "\\\\wsl.localhost\\Ubuntu\\");
    }

    #[test]
    fn from_unc_accepts_this_distro_under_either_host_and_nothing_else() {
        let w = ubuntu();
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Ubuntu\\home\\u\\p").as_deref(), Some("/home/u/p"));
        assert_eq!(w.from_unc("\\\\wsl$\\ubuntu\\home\\u\\").as_deref(), Some("/home/u"), "case-insensitive, trailing separator dropped");
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Ubuntu\\").as_deref(), Some("/"));
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Ubuntu").as_deref(), Some("/"));
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Debian\\home"), None, "a foreign distro is not ours to launch into");
        assert_eq!(w.from_unc("\\\\server\\share\\x"), None);
        assert_eq!(w.from_unc("/home/u"), None);
    }

    #[test]
    fn ps_rows_parse_with_padding_and_a_space_in_the_command_name() {
        let out = "    1     0  0.0  1024 init\n\
                   4242  1000 12.5 51200 claude\n\
                   4300  4242  0.3  4096 my helper\n\
                   junk line\n";
        let rows = parse_ps(out);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].pid, 4242);
        assert_eq!(rows[1].ppid, 1000);
        assert_eq!(rows[1].cpu, 12.5);
        assert_eq!(rows[1].rss_kb, 51200);
        assert_eq!(rows[1].name, "claude");
        assert_eq!(rows[2].name, "my helper", "comm is last so its spaces cannot shift the numbers");
    }
}

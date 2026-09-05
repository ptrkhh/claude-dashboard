//! One Claude Code installation the agent can see, and the routing that picks
//! one for a launch. Linux has one side. Windows has the native side and, when
//! the boot probe succeeds, a second side reached through `wsl.exe` and the
//! `\\wsl.localhost` share.

use super::validate::BadRequest;
use crate::host::cmd::Runner;
use crate::host::proc::ProcRow;
use crate::host::sample::{SampledUsage, Sampler};
use crate::host::wsl::{WslPaths, MISSING_SCRIPT};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    /// Sessions live in tmux: `list-panes`, `new-session`, `kill-session`.
    Tmux,
    /// Sessions live in their own console window; ownership goes by `--name`.
    #[cfg(windows)]
    Console,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Procs {
    /// `sysinfo` through the shared sampler: the kernel we run on.
    Sampler,
    /// One `ps` per poll through the side's runner: a kernel we do not.
    #[cfg(windows)]
    Ps,
}

pub struct Side {
    /// `~/.claude`, `C:\Users\u\.claude`, or `\\wsl.localhost\Ubuntu\home\u\.claude`.
    pub claude_dir: PathBuf,
    /// Native, or `wsl.exe`-prefixed (Task 3).
    pub runner: Arc<Runner>,
    pub backend: Backend,
    pub procs: Procs,
    /// Set on WSL sides only: the share root and the distro name.
    pub wsl: Option<WslPaths>,
}

impl Side {
    pub fn native(claude_dir: PathBuf, runner: Arc<Runner>) -> Self {
        Side {
            claude_dir,
            runner,
            #[cfg(windows)]
            backend: Backend::Console,
            #[cfg(not(windows))]
            backend: Backend::Tmux,
            procs: Procs::Sampler,
            wsl: None,
        }
    }

    /// From a successful boot probe (Task 9). `None` only when the home line
    /// is not a share path this crate understands.
    #[cfg(windows)]
    pub fn wsl(
        probe: &crate::host::wsl::WslProbe,
        log: Arc<crate::host::log::LogBuffer>,
    ) -> Option<Self> {
        let paths = WslPaths::from_home_unc(&probe.home_unc)?;
        let prefix = crate::host::wsl::wsl_prefix(probe.distro_flag.as_deref(), &probe.path);
        // The runner's own PATH stays the Windows one: it is what finds wsl.exe.
        let runner = Runner::with_prefix(
            prefix,
            "wsl ",
            std::env::var("PATH").unwrap_or_default(),
            log,
        );
        Some(Side {
            claude_dir: PathBuf::from(format!("{}\\.claude", probe.home_unc)),
            runner: Arc::new(runner),
            backend: Backend::Tmux,
            procs: Procs::Ps,
            wsl: Some(paths),
        })
    }

    pub fn is_wsl(&self) -> bool {
        self.wsl.is_some()
    }

    /// The process rows this side's sessions are checked against, once per poll.
    pub async fn proc_rows(&self, sampler: &Mutex<Sampler>) -> Vec<ProcRow> {
        match self.procs {
            Procs::Sampler => sampler.lock().unwrap_or_else(|e| e.into_inner()).sample(),
            #[cfg(windows)]
            Procs::Ps => crate::host::wsl::parse_ps(
                &self.runner.run("ps", crate::host::wsl::PS_ARGS, "wsl ps").await,
            ),
        }
    }

    /// CPU and RSS of the tree under `pid`. The `ps` figure is a lifetime
    /// average, reported as `Some` with no sample age, as the Node agent did.
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn tree_usage(&self, sampler: &Mutex<Sampler>, rows: &[ProcRow], pid: i32) -> SampledUsage {
        match self.procs {
            Procs::Sampler => sampler.lock().unwrap_or_else(|e| e.into_inner()).tree_usage(pid),
            #[cfg(windows)]
            Procs::Ps => {
                let u = crate::host::proc::proc_tree_usage(rows, pid);
                SampledUsage { cpu: Some(u.cpu), rss_kb: u.rss_kb, cpu_sample_age_ms: 0 }
            }
        }
    }

    /// Binaries this side lacks, re-probed per call like the native list.
    /// `run_checked` and not `run`: the swallowing `run` reports a stopped,
    /// hung or absent distro as the empty list a fully-equipped side returns.
    /// A side that could not be reached at all answers `["wsl unreachable"]`.
    pub async fn wsl_missing(&self) -> Vec<String> {
        match self.runner.run_checked("sh", &["-c", MISSING_SCRIPT], "wsl missing").await {
            Ok(out) => out.lines().map(str::to_string).collect(),
            Err(_) => vec!["wsl unreachable".to_string()],
        }
    }
}

/// The three shapes a directory can arrive in. String checks, not
/// `Path::is_absolute`: on Windows `/x` is not absolute, yet it is exactly how
/// a WSL directory is named.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// `C:\…` or `C:/…`
    Drive,
    /// `\\host\…`
    Unc,
    /// `/…`
    Posix,
}

pub fn shape_of(p: &str) -> Option<Shape> {
    let b = p.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        Some(Shape::Drive)
    } else if b.len() > 2 && p.starts_with("\\\\") {
        Some(Shape::Unc)
    } else if p.starts_with('/') {
        Some(Shape::Posix)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// The native side, directory as given.
    Native(String),
    /// The WSL side, directory in Linux notation.
    Wsl(String),
}

fn bad(p: &str, why: &str) -> BadRequest {
    BadRequest(format!("bad path: {p} ({why})"))
}

/// Spec §4, the Windows column. Compiled everywhere so the table is tested
/// everywhere; `side_for` selects the platform's own.
pub fn route_windows(wsl: Option<&WslPaths>, dir: &str) -> Result<Route, BadRequest> {
    match shape_of(dir) {
        Some(Shape::Drive) => Ok(Route::Native(dir.to_string())),
        Some(Shape::Unc) => {
            let w = wsl.ok_or_else(|| bad(dir, "no WSL side"))?;
            w.from_unc(dir).map(Route::Wsl).ok_or_else(|| bad(dir, "not the configured distro"))
        }
        Some(Shape::Posix) => {
            wsl.ok_or_else(|| bad(dir, "no WSL side"))?;
            Ok(Route::Wsl(dir.to_string()))
        }
        None => Err(bad(dir, "not absolute")),
    }
}

/// Spec §4, the Unix column.
pub fn route_unix(dir: &str) -> Result<Route, BadRequest> {
    match shape_of(dir) {
        Some(Shape::Posix) => Ok(Route::Native(dir.to_string())),
        _ => Err(BadRequest(format!("bad path: {dir}"))),
    }
}

/// The side a launch lands on and the directory in that side's notation.
/// `sides[0]` is always the native side (`Ctx::new` guarantees one).
pub fn side_for<'a>(sides: &'a [Side], dir: &str) -> Result<(&'a Side, String), BadRequest> {
    #[cfg(windows)]
    let route = route_windows(sides.iter().find_map(|s| s.wsl.as_ref()), dir)?;
    #[cfg(not(windows))]
    let route = route_unix(dir)?;
    match route {
        Route::Native(d) => Ok((&sides[0], d)),
        Route::Wsl(d) => sides
            .iter()
            .find(|s| s.is_wsl())
            .map(|s| (s, d))
            .ok_or_else(|| bad(dir, "no WSL side")),
    }
}

/// Shape only, for favourites: a path a launch could route is a path worth
/// remembering. Whether a side exists for it is a launch-time question.
pub fn path_is_valid(p: &str) -> bool {
    #[cfg(windows)]
    {
        shape_of(p).is_some()
    }
    #[cfg(not(windows))]
    {
        shape_of(p) == Some(Shape::Posix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn ubuntu() -> WslPaths {
        WslPaths { unc_root: "\\\\wsl.localhost\\Ubuntu".into(), distro: "Ubuntu".into() }
    }

    #[test]
    fn shapes_are_string_checks() {
        assert_eq!(shape_of("C:\\Users\\u"), Some(Shape::Drive));
        assert_eq!(shape_of("d:/git"), Some(Shape::Drive));
        assert_eq!(shape_of("\\\\wsl.localhost\\Ubuntu\\home"), Some(Shape::Unc));
        assert_eq!(shape_of("/home/u"), Some(Shape::Posix));
        assert_eq!(shape_of("relative/x"), None);
        assert_eq!(shape_of(""), None);
        assert_eq!(shape_of("C:"), None, "a bare drive letter is not a directory");
    }

    #[test]
    fn the_windows_routing_table() {
        let w = ubuntu();
        assert_eq!(route_windows(Some(&w), "C:\\p").unwrap(), Route::Native("C:\\p".into()));
        assert_eq!(route_windows(None, "C:\\p").unwrap(), Route::Native("C:\\p".into()));
        assert_eq!(
            route_windows(Some(&w), "\\\\wsl.localhost\\Ubuntu\\home\\u").unwrap(),
            Route::Wsl("/home/u".into())
        );
        assert_eq!(route_windows(Some(&w), "/home/u").unwrap(), Route::Wsl("/home/u".into()));
        assert!(route_windows(None, "/home/u").is_err(), "a / path needs a WSL side");
        assert!(route_windows(None, "\\\\wsl.localhost\\Ubuntu\\home").is_err());
        assert!(route_windows(Some(&w), "\\\\wsl.localhost\\Debian\\home").is_err(), "another distro");
        assert!(route_windows(Some(&w), "\\\\server\\share").is_err());
        assert!(route_windows(Some(&w), "relative").is_err());
        let e = route_windows(None, "/x").unwrap_err();
        assert!(e.0.starts_with("bad path: /x"), "{e:?}");
    }

    #[test]
    fn the_unix_routing_table() {
        assert_eq!(route_unix("/home/u").unwrap(), Route::Native("/home/u".into()));
        assert!(route_unix("C:\\p").is_err());
        assert!(route_unix("\\\\wsl.localhost\\Ubuntu\\home").is_err());
        assert!(route_unix("relative").is_err());
    }

    #[test]
    fn side_for_picks_the_native_side_on_its_own_platform() {
        let log = Arc::new(LogBuffer::new());
        let runner = Arc::new(Runner::new(String::new(), log));
        let sides = vec![Side::native(PathBuf::from("/tmp/.claude"), runner)];
        let native_dir = if cfg!(windows) { "C:\\p" } else { "/p" };
        let (s, d) = side_for(&sides, native_dir).unwrap();
        assert!(!s.is_wsl());
        assert_eq!(d, native_dir);
        assert!(side_for(&sides, "relative").is_err());
    }

    #[tokio::test]
    async fn an_unreachable_side_is_not_a_fully_equipped_one() {
        // The defect this closes: `Runner::run` swallows, so a distro that is
        // stopped or gone printed nothing — exactly what a distro with tmux,
        // claude and git all installed prints.
        let log = Arc::new(LogBuffer::new());
        let dead = Side::native(
            PathBuf::from("/tmp/.claude"),
            Arc::new(Runner::new("/nonexistent-dir-for-test".to_string(), Arc::clone(&log))),
        );
        assert_eq!(dead.wsl_missing().await, vec!["wsl unreachable".to_string()]);

        // A side that answered is never reported unreachable, whichever of the
        // three binaries this host happens to have. Unix only: the probe runs
        // `sh`, which a bare Windows runner resolves only if Git for Windows
        // happens to be on PATH.
        #[cfg(unix)]
        {
            let live = Side::native(
                PathBuf::from("/tmp/.claude"),
                Arc::new(Runner::new(std::env::var("PATH").unwrap_or_default(), log)),
            );
            assert!(!live.wsl_missing().await.contains(&"wsl unreachable".to_string()));
        }
    }

    #[test]
    fn a_favorite_path_has_one_of_the_platform_shapes() {
        assert!(path_is_valid("/home/u"));
        assert!(!path_is_valid("relative/x"));
        assert!(!path_is_valid(""));
        assert_eq!(path_is_valid("C:\\Users"), cfg!(windows));
        assert_eq!(path_is_valid("\\\\wsl.localhost\\Ubuntu\\home"), cfg!(windows));
    }
}

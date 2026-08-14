use super::proc::{proc_tree_usage, ProcRow};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`. Two refreshes closer together than
/// this return a deflated CPU number, not an error and not a zero.
pub const MIN_CPU_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq)]
pub struct SampledUsage {
    /// `None` until two refreshes at least `MIN_CPU_INTERVAL` apart have run.
    pub cpu: Option<f32>,
    pub rss_kb: u64,
    pub cpu_sample_age_ms: u128,
}

/// Holds a long-lived `System` across requests. `collectSessions` is
/// request-driven, so without this every call would be a first refresh and
/// every CPU number would be zero.
pub struct Sampler {
    sys: System,
    last_refresh: Option<Instant>,
    cpu_valid: bool,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self { sys: System::new(), last_refresh: None, cpu_valid: false }
    }

    /// Refresh only when the previous sample is at least `MIN_CPU_INTERVAL`
    /// old. A sub-interval refresh would deflate the CPU figure.
    fn refresh_if_due(&mut self) {
        let due = match self.last_refresh {
            None => true,
            Some(t) => t.elapsed() >= MIN_CPU_INTERVAL,
        };
        if !due {
            return;
        }
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        // The first refresh establishes a baseline; only from the second
        // onward is cpu_usage() meaningful.
        if self.last_refresh.is_some() {
            self.cpu_valid = true;
        }
        self.last_refresh = Some(Instant::now());
    }

    pub fn sample(&mut self) -> Vec<ProcRow> {
        self.refresh_if_due();
        self.sys
            .processes()
            .values()
            .map(|p| ProcRow {
                pid: p.pid().as_u32() as i32,
                ppid: p.parent().map(|x| x.as_u32() as i32).unwrap_or(0),
                cpu: p.cpu_usage(),
                rss_kb: p.memory() / 1024, // sysinfo reports bytes; Node reported KiB
            })
            .collect()
    }

    pub fn tree_usage(&mut self, root_pid: i32) -> SampledUsage {
        let rows = self.sample();
        let usage = proc_tree_usage(&rows, root_pid);
        SampledUsage {
            cpu: if self.cpu_valid { Some(usage.cpu) } else { None },
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: self
                .last_refresh
                .map(|t| t.elapsed().as_millis())
                .unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_reports_cpu_as_none_not_zero() {
        // A single refresh cannot produce a CPU number. Reporting 0.0 would be
        // a plausible lie; None is the honest answer.
        let mut s = Sampler::new();
        let u = s.tree_usage(std::process::id() as i32);
        assert_eq!(u.cpu, None);
        assert!(u.rss_kb > 0, "RSS is available on the first refresh");
    }

    #[test]
    fn a_second_sample_after_the_interval_produces_a_cpu_number() {
        let mut s = Sampler::new();
        let _ = s.tree_usage(std::process::id() as i32);
        std::thread::sleep(MIN_CPU_INTERVAL + Duration::from_millis(50));
        let u = s.tree_usage(std::process::id() as i32);
        assert!(u.cpu.is_some(), "cpu must be Some after a >=200ms gap");
        assert!(u.cpu.unwrap() >= 0.0);
    }

    #[test]
    fn a_sub_interval_call_serves_the_last_good_sample_rather_than_resampling() {
        // The threshold governs when to RE-SAMPLE, not what to serve. An
        // imperative poll moments after a good sample must not render a dash.
        let mut s = Sampler::new();
        let _ = s.tree_usage(std::process::id() as i32);
        std::thread::sleep(MIN_CPU_INTERVAL + Duration::from_millis(50));
        let first = s.tree_usage(std::process::id() as i32);
        let second = s.tree_usage(std::process::id() as i32); // immediate
        assert_eq!(first.cpu, second.cpu);
        assert!(second.cpu_sample_age_ms < MIN_CPU_INTERVAL.as_millis());
    }

    #[test]
    fn unknown_pid_yields_zero_rss() {
        let mut s = Sampler::new();
        let u = s.tree_usage(-1);
        assert_eq!(u.rss_kb, 0);
    }
}

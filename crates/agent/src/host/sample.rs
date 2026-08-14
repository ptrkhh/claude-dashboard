use super::proc::{proc_tree_usage, ProcRow};
use serde::Serialize;
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MachineStats {
    #[serde(rename = "cpuPct")]
    pub cpu_pct: u32,
    #[serde(rename = "ramUsedKb")]
    pub ram_used_kb: u64,
    #[serde(rename = "ramTotalKb")]
    pub ram_total_kb: u64,
}

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

    /// Ports `machineStats` (`lib/stats.js:32-35`). `available_parallelism` is
    /// the logical-CPU count `os.cpus().length` reported, and needs no
    /// `System` refresh. Byte-to-KiB conversion rounds rather than truncating,
    /// because Node's `Math.round` did.
    pub fn machine_stats(&mut self) -> MachineStats {
        self.sys.refresh_memory();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        let load = System::load_average().one;
        let pct = ((load / cores) * 100.0).round();
        let to_kb = |bytes: u64| (bytes as f64 / 1024.0).round() as u64;
        let total = self.sys.total_memory();
        MachineStats {
            cpu_pct: pct.clamp(0.0, 100.0) as u32,
            ram_used_kb: to_kb(total.saturating_sub(self.sys.free_memory())),
            ram_total_kb: to_kb(total),
        }
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

    #[test]
    fn machine_stats_reports_plausible_ram_and_a_clamped_cpu() {
        let mut s = Sampler::new();
        let m = s.machine_stats();
        assert!(m.ram_total_kb > 0);
        assert!(m.ram_used_kb <= m.ram_total_kb);
        // Node: Math.min(100, ...) — a load average above core count must not
        // render a 340% CPU bar.
        assert!(m.cpu_pct <= 100);
    }

    #[test]
    fn machine_stats_serializes_with_nodes_field_names() {
        let m = MachineStats { cpu_pct: 12, ram_used_kb: 3, ram_total_kb: 4 };
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"cpuPct\":12"));
        assert!(j.contains("\"ramUsedKb\":3"));
        assert!(j.contains("\"ramTotalKb\":4"));
    }
}

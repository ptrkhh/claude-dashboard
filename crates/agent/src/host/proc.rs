use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub pid: i32,
    pub ppid: i32,
    pub cpu: f32,
    pub rss_kb: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeUsage {
    pub cpu: f32,
    pub rss_kb: u64,
}

pub fn proc_tree_usage(rows: &[ProcRow], root_pid: i32) -> TreeUsage {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for r in rows {
        children.entry(r.ppid).or_default().push(r.pid);
    }
    let by_pid: HashMap<i32, &ProcRow> = rows.iter().map(|r| (r.pid, r)).collect();

    let mut cpu = 0.0f32;
    let mut rss_kb = 0u64;
    let mut seen: HashSet<i32> = HashSet::new();
    let mut stack: Vec<i32> = if by_pid.contains_key(&root_pid) {
        vec![root_pid]
    } else {
        vec![]
    };

    while let Some(pid) = stack.pop() {
        // The Node version has no cycle guard; a ppid loop would hang it.
        if !seen.insert(pid) {
            continue;
        }
        if let Some(r) = by_pid.get(&pid) {
            cpu += r.cpu;
            rss_kb += r.rss_kb;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }

    // Node: Math.round(cpu * 10) / 10
    TreeUsage { cpu: (cpu * 10.0).round() / 10.0, rss_kb }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ProcRow> {
        vec![
            ProcRow { pid: 1, ppid: 0, cpu: 0.0, rss_kb: 1000 },
            ProcRow { pid: 100, ppid: 1, cpu: 5.0, rss_kb: 50000 },   // root
            ProcRow { pid: 200, ppid: 100, cpu: 10.0, rss_kb: 20000 }, // child
            ProcRow { pid: 300, ppid: 200, cpu: 1.5, rss_kb: 4000 },   // grandchild
            ProcRow { pid: 400, ppid: 1, cpu: 9.9, rss_kb: 99999 },    // unrelated
        ]
    }

    #[test]
    fn sums_the_tree_rooted_at_pid() {
        let u = proc_tree_usage(&rows(), 100);
        assert_eq!(u.cpu, 16.5);
        assert_eq!(u.rss_kb, 74000);
    }

    #[test]
    fn unknown_pid_yields_zeros() {
        let u = proc_tree_usage(&rows(), 999);
        assert_eq!(u.cpu, 0.0);
        assert_eq!(u.rss_kb, 0);
    }

    #[test]
    fn a_parent_cycle_terminates() {
        // Defensive: /proc can race such that ppid chains form a loop.
        let cyclic = vec![
            ProcRow { pid: 10, ppid: 11, cpu: 1.0, rss_kb: 10 },
            ProcRow { pid: 11, ppid: 10, cpu: 2.0, rss_kb: 20 },
        ];
        let u = proc_tree_usage(&cyclic, 10);
        assert_eq!(u.rss_kb, 30);
    }
}

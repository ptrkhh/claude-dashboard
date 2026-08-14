use cdash_agent::host::sample::{Sampler, MIN_CPU_INTERVAL};
use std::time::Duration;

fn main() {
    let pid = std::process::id() as i32;
    let mut s = Sampler::new();
    println!("first:      {:?}", s.tree_usage(pid).cpu);
    std::thread::sleep(Duration::from_millis(50));
    println!("after 50ms: {:?}", s.tree_usage(pid).cpu);
    std::thread::sleep(MIN_CPU_INTERVAL + Duration::from_millis(50));
    println!("after 250ms:{:?}", s.tree_usage(pid).cpu);
}

//! Process resource sampling: the htop's input.
//!
//! Over the whole process tree, not the direct child. Under Wine the process
//! Cellar spawns is `wine`, and the cpu and memory that matter belong to
//! `sbox-server.exe` beneath it. Sampling only the child reports near zero and
//! the dashboard confidently shows an idle server under full load.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use cellar_core::event::ResourceSample;
use sysinfo::{Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// Samples a process tree, keeping the `sysinfo` state that cpu percentages need.
///
/// A cpu reading is a delta between two refreshes, so the first sample after
/// construction is meaningless and is reported as zero rather than as a spike.
pub struct Sampler {
    system: System,
    networks: Networks,
    primed: bool,
    last_network: Option<(Instant, u64, u64)>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            system: System::new_with_specifics(
                RefreshKind::new()
                    .with_processes(ProcessRefreshKind::new().with_cpu().with_memory()),
            ),
            networks: Networks::new_with_refreshed_list(),
            primed: false,
            last_network: None,
        }
    }

    /// Sample the tree rooted at `root_pid`.
    ///
    /// Returns `None` when the root is gone, which is how a caller learns the
    /// process died between the exit check and the sample.
    pub fn sample(&mut self, root_pid: u32) -> Option<ResourceSample> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cpu().with_memory(),
        );

        let root = Pid::from_u32(root_pid);
        self.system.process(root)?;
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.networks.refresh();

        let members = self.tree_of(root);

        let mut cpu_percent = 0.0f32;
        let mut memory_bytes = 0u64;

        for pid in &members {
            if let Some(process) = self.system.process(*pid) {
                cpu_percent += process.cpu_usage();
                memory_bytes += process.memory();
            }
        }

        let first = !self.primed;
        self.primed = true;

        let now = Instant::now();
        let received: u64 = self.networks.values().map(|data| data.received()).sum();
        let transmitted: u64 = self.networks.values().map(|data| data.transmitted()).sum();
        let (network_rx_bytes_per_sec, network_tx_bytes_per_sec) = self
            .last_network
            .map(|(at, old_received, old_transmitted)| {
                let seconds = now.duration_since(at).as_secs_f64().max(0.001);
                (
                    ((received.saturating_sub(old_received) as f64) / seconds) as u64,
                    ((transmitted.saturating_sub(old_transmitted) as f64) / seconds) as u64,
                )
            })
            .unwrap_or_default();
        self.last_network = Some((now, received, transmitted));
        let total_memory = self.system.total_memory();
        let host_memory_percent = if total_memory == 0 {
            0.0
        } else {
            self.system.used_memory() as f32 * 100.0 / total_memory as f32
        };
        let cpu_core_count = self.system.cpus().len().max(1);
        let process_cpu_percent = if first { 0.0 } else { cpu_percent };

        Some(ResourceSample {
            at: chrono::Utc::now(),
            // A cpu percentage needs two refreshes to mean anything.
            cpu_percent: process_cpu_percent,
            cpu_percent_all_cores: process_cpu_percent / cpu_core_count as f32,
            cpu_core_count,
            memory_bytes,
            process_count: members.len(),
            host_cpu_percent: if first {
                0.0
            } else {
                self.system.global_cpu_usage()
            },
            host_memory_percent,
            network_rx_bytes_per_sec,
            network_tx_bytes_per_sec,
        })
    }

    /// Every pid in the tree rooted at `root`, the root included.
    fn tree_of(&self, root: Pid) -> HashSet<Pid> {
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in self.system.processes() {
            if let Some(parent) = process.parent() {
                children.entry(parent).or_default().push(*pid);
            }
        }

        let mut members = HashSet::new();
        let mut queue = vec![root];

        // A `seen` set as well as `members`, because a pid table can contain a
        // cycle after pid reuse, and a cycle here is an infinite loop in the
        // sampler rather than a wrong number.
        while let Some(pid) = queue.pop() {
            if !members.insert(pid) {
                continue;
            }
            if let Some(kids) = children.get(&pid) {
                queue.extend(kids.iter().copied());
            }
        }

        members
    }
}

/// Bytes as a short human string, for the TUI and the CLI.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Seconds as `4h12m`, the shape a status line wants.
pub fn format_uptime(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (days, hours) = (seconds / 86_400, (seconds % 86_400) / 3600);
    let (minutes, secs) = ((seconds % 3600) / 60, seconds % 60);

    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn sampling_this_process_reports_something_plausible() {
        let mut sampler = Sampler::new();
        let me = std::process::id();

        // The first sample is primed and reports zero cpu by design.
        let first = sampler.sample(me).unwrap();
        assert_eq!(first.cpu_percent, 0.0);
        assert!(first.memory_bytes > 0, "this process uses some memory");
        assert!(first.process_count >= 1);

        let second = sampler.sample(me).unwrap();
        assert!(second.memory_bytes > 0);
    }

    #[test]
    fn a_dead_root_samples_as_nothing_rather_than_as_zero() {
        let mut sampler = Sampler::new();

        // Pid 0 is not a usable stand-in for "dead": it is the kernel idle
        // process, and on Windows sysinfo reports it as a live entry, so the
        // sample would come back Some and this test would lie about the
        // platform it just failed on. Spawn something real and wait for it
        // to exit instead, so the pid is genuinely gone rather than reserved.
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit"])
            .spawn()
            .unwrap();
        #[cfg(not(windows))]
        let mut child = std::process::Command::new("true").spawn().unwrap();

        let dead_pid = child.id();
        child.wait().unwrap();

        assert!(sampler.sample(dead_pid).is_none());
    }

    #[test]
    fn the_tree_includes_children_not_just_the_root() {
        let mut sampler = Sampler::new();
        sampler.sample(std::process::id());

        // The whole tree from pid 1 on unix, or the root of this session on
        // windows, must be larger than one process on any real machine.
        #[cfg(unix)]
        {
            let all = sampler.tree_of(Pid::from_u32(1));
            assert!(all.len() > 1, "the init tree has more than one process");
        }
    }

    #[test]
    fn byte_formatting_reads_the_way_a_dashboard_wants() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GiB");
    }

    #[test]
    fn uptime_formatting_drops_units_it_does_not_need() {
        assert_eq!(format_uptime(5), "5s");
        assert_eq!(format_uptime(65), "1m05s");
        assert_eq!(format_uptime(4 * 3600 + 12 * 60), "4h12m");
        assert_eq!(format_uptime(2 * 86_400 + 3 * 3600), "2d3h");
        assert_eq!(format_uptime(-1), "0s");
    }
}

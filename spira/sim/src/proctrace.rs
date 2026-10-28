use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const TRACE_ENV: &str = "SIM_PROC_TRACE";
pub const TRACE_FILE: &str = "proc-trace.log";
const SCAN: Duration = Duration::from_millis(3);

pub struct Seen {
    pub pid: u32,
    pub ppid: u32,
    pub first_ms: u128,
    pub last_ms: u128,
    pub cpu_ms: u64,
    pub child_cpu_ms: u64,
    pub argv: String,
}

pub struct Tracer {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Vec<Seen>>,
}

pub fn start(root: u32) -> Tracer {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut seen: BTreeMap<u32, Seen> = BTreeMap::new();
        loop {
            let done = flag.load(Ordering::Relaxed);
            scan(root, &mut seen, t0);
            if done {
                break;
            }
            std::thread::sleep(SCAN);
        }
        seen.into_values().collect()
    });
    Tracer { stop, handle }
}

impl Tracer {
    pub fn finish(self) -> Vec<Seen> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or_default()
    }
}

fn stat(pid: u32) -> Option<(u32, u64, u64)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest: Vec<&str> = s.rsplit_once(')')?.1.split_whitespace().collect();
    let ppid = rest.get(1)?.parse().ok()?;
    let own = rest.get(11)?.parse::<u64>().ok()? + rest.get(12)?.parse::<u64>().ok()?;
    let children = rest.get(13)?.parse::<u64>().ok()? + rest.get(14)?.parse::<u64>().ok()?;
    Some((ppid, own * 10, children * 10))
}

fn argv(pid: u32) -> String {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    String::from_utf8_lossy(&raw).split('\0').filter(|a| !a.is_empty()).collect::<Vec<_>>().join(" ")
}

fn scan(root: u32, seen: &mut BTreeMap<u32, Seen>, t0: Instant) {
    let now = t0.elapsed().as_millis();
    let mut all: Vec<(u32, u32, u64, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            if let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) {
                if let Some((ppid, cpu, kids)) = stat(pid) {
                    all.push((pid, ppid, cpu, kids));
                }
            }
        }
    }
    let mut live: BTreeMap<u32, (u32, u64, u64)> = BTreeMap::new();
    let mut grew = true;
    while grew {
        grew = false;
        for &(pid, ppid, cpu, kids) in &all {
            if !live.contains_key(&pid) && (pid == root || live.contains_key(&ppid)) {
                live.insert(pid, (ppid, cpu, kids));
                grew = true;
            }
        }
    }
    for (pid, (ppid, cpu, kids)) in live {
        let e = seen.entry(pid).or_insert_with(|| Seen { pid, ppid, first_ms: now, last_ms: now, cpu_ms: 0, child_cpu_ms: 0, argv: String::new() });
        e.last_ms = now;
        e.cpu_ms = cpu;
        e.child_cpu_ms = kids;
        let a = argv(pid);
        if !a.is_empty() {
            e.argv = a;
        }
    }
}

pub fn render(label: &str, seen: &[Seen]) -> String {
    let mut s = format!("=== {label}\n");
    for e in seen {
        s.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\n", e.first_ms, e.last_ms - e.first_ms, e.cpu_ms, e.child_cpu_ms, e.pid, e.ppid, e.argv.chars().take(160).collect::<String>()));
    }
    s
}

struct Row {
    life: u64,
    own: u64,
    kids: u64,
    pid: u32,
    ppid: u32,
    name: String,
}

fn name_of(argv: &str) -> String {
    let mut words = argv.split_whitespace();
    let program = words.next().unwrap_or("?").rsplit('/').next().unwrap_or("?").to_string();
    if !matches!(program.as_str(), "sh" | "bash" | "timeout" | "env") {
        return program;
    }
    match words.find(|w| !w.starts_with('-') && !w.starts_with(|c: char| c.is_ascii_digit())) {
        Some(next) => format!("{program} {}", next.rsplit('/').next().unwrap_or(next)),
        None => program,
    }
}

/// What a trace says per program: how many were seen, their summed lifetime and own CPU, the
/// CPU their reaped children spent beyond the children the sampler caught (the processes too
/// short-lived to see), and the programs that started those. One line each, costliest first.
pub fn summarize(trace: &str) -> String {
    let rows: Vec<Row> = trace
        .lines()
        .filter(|l| !l.starts_with("=== "))
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(7, '\t').collect();
            Some(Row { life: f.get(1)?.parse().ok()?, own: f.get(2)?.parse().ok()?, kids: f.get(3)?.parse().ok()?, pid: f.get(4)?.parse().ok()?, ppid: f.get(5)?.parse().ok()?, name: name_of(f.get(6)?) })
        })
        .collect();
    let mut caught: BTreeMap<u32, u64> = BTreeMap::new();
    for r in &rows {
        *caught.entry(r.ppid).or_default() += r.own + r.kids;
    }
    let mut by_name: BTreeMap<&str, (u64, u64, u64, u64)> = BTreeMap::new();
    for r in &rows {
        let e = by_name.entry(&r.name).or_default();
        e.0 += 1;
        e.1 += r.life;
        e.2 += r.own;
        e.3 += r.kids.saturating_sub(caught.get(&r.pid).copied().unwrap_or(0));
    }
    let mut lines: Vec<(u64, String)> = by_name
        .iter()
        .map(|(n, (count, life, own, unseen))| (own + unseen, format!("{count:>5} procs {life:>8} ms alive {own:>7} ms cpu {unseen:>7} ms cpu in children too short to see: {n}")))
        .collect();
    lines.sort_by_key(|l| std::cmp::Reverse(l.0));
    lines.into_iter().map(|(_, l)| l + "\n").collect()
}

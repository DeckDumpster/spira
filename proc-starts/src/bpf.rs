//! The kernel side: load the object, attach its tracepoints, then read the maps through held fds.
//! Reading and deleting map elements through an fd needs no capability; creating objects does.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::path::Path;
use std::time::Duration;

use aya::maps::Map;
use aya::programs::BtfTracePoint;
use aya::{Btf, Ebpf};
use spira_otel::metrics::Series;

use crate::run::Host;
use crate::totals::{cpu_seconds, host_forks, Drain, StartCount};

#[repr(C, align(8))]
struct Aligned<T: ?Sized>(T);

// The loader reads ELF64 headers in place, which wants 8-byte alignment.
static ALIGNED: &Aligned<[u8]> = &Aligned(*include_bytes!("../bpf/proc_starts.bpf.o"));

const PROGRAMS: [(&str, &str); 4] = [("on_fork", "sched_process_fork"), ("on_exec", "sched_process_exec"), ("on_exit", "sched_process_exit"), ("on_mkdir", "cgroup_mkdir")];

const START_KEY: usize = 40;
const COMM: usize = 16;
const PATH: usize = 256;
const CACHE_MAX: usize = 20_000;

const BPF_MAP_LOOKUP_ELEM: libc::c_int = 1;
const BPF_MAP_GET_NEXT_KEY: libc::c_int = 4;
const BPF_MAP_LOOKUP_AND_DELETE_ELEM: libc::c_int = 21;

#[repr(C)]
struct ElemAttr {
    map_fd: u32,
    pad: u32,
    key: u64,
    value: u64,
    flags: u64,
}

fn elem(cmd: libc::c_int, fd: RawFd, key: *const u8, value: *mut u8) -> io::Result<()> {
    let mut a = ElemAttr { map_fd: fd as u32, pad: 0, key: key as u64, value: value as u64, flags: 0 };
    // SAFETY: `a` is the leading part of union bpf_attr the kernel reads for element commands;
    // key and value point at buffers sized to the map's key and value.
    let r = unsafe { libc::syscall(libc::SYS_bpf, cmd, &mut a as *mut ElemAttr, std::mem::size_of::<ElemAttr>()) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn keys(fd: RawFd, key_size: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut next = vec![0u8; key_size];
    let mut prev: Option<Vec<u8>> = None;
    while elem(BPF_MAP_GET_NEXT_KEY, fd, prev.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()), next.as_mut_ptr()).is_ok() {
        out.push(next.clone());
        prev = Some(next.clone());
    }
    out
}

fn lookup(fd: RawFd, key: &[u8], value_size: usize, delete: bool) -> Option<Vec<u8>> {
    let mut v = vec![0u8; value_size];
    let cmd = if delete { BPF_MAP_LOOKUP_AND_DELETE_ELEM } else { BPF_MAP_LOOKUP_ELEM };
    elem(cmd, fd, key.as_ptr(), v.as_mut_ptr()).ok().map(|_| v)
}

/// Empties a map of `u64` counts keyed by cgroup id.
fn take_counts(fd: RawFd) -> Vec<(u64, u64)> {
    keys(fd, 8).into_iter().filter_map(|k| lookup(fd, &k, 8, true).map(|v| (u64_at(&k, 0), u64_at(&v, 0)))).collect()
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn comm(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn fd_of(map: &Map) -> RawFd {
    let data = match map {
        Map::HashMap(d) | Map::Array(d) | Map::PerCpuArray(d) | Map::LruHashMap(d) => d,
        _ => unreachable!("the object declares only these map types"),
    };
    data.fd().as_fd().as_raw_fd()
}

pub struct Kernel {
    _ebpf: Ebpf,
    maps: [Map; 7],
    cpus: usize,
    release_root: String,
    clk_tck: f64,
    cache: HashMap<u64, String>,
    walked: bool,
}

impl Kernel {
    /// Loads and attaches every program. Needs CAP_BPF and CAP_PERFMON.
    pub fn load(release_root: String) -> Result<Kernel, String> {
        let mut ebpf = Ebpf::load(&ALIGNED.0).map_err(|e| format!("cannot load the bpf object: {e}"))?;
        let btf = Btf::from_sys_fs().map_err(|e| format!("cannot read the kernel's BTF: {e}"))?;
        for (prog, tracepoint) in PROGRAMS {
            let p: &mut BtfTracePoint = ebpf.program_mut(prog).ok_or(format!("object has no program {prog}"))?.try_into().map_err(|e| format!("{prog}: {e}"))?;
            p.load(tracepoint, &btf).map_err(|e| format!("cannot load {prog} on {tracepoint}: {e}"))?;
            p.attach().map_err(|e| format!("cannot attach {prog} to {tracepoint}: {e}"))?;
        }
        let mut take = |name: &str| ebpf.take_map(name).ok_or(format!("object has no map {name}"));
        let maps = [take("starts")?, take("threads")?, take("names")?, take("dropped")?, take("cost")?, take("pending")?, take("execs")?];
        let cpus = aya::util::nr_cpus().map_err(|(what, e)| format!("{what}: {e}"))? as usize;
        // SAFETY: sysconf reads nothing from memory.
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        Ok(Kernel { _ebpf: ebpf, maps, cpus, release_root, clk_tck, cache: HashMap::new(), walked: false })
    }

    fn walk(&mut self, dir: &Path, path: &str) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let child = format!("{path}/{}", e.file_name().to_string_lossy());
            self.cache.insert(std::os::unix::fs::DirEntryExt::ino(&e), child.clone());
            self.walk(&e.path(), &child);
        }
    }

    fn named(&self, cgroup: u64) -> Option<String> {
        let v = lookup(fd_of(&self.maps[2]), &cgroup.to_ne_bytes(), PATH, false)?;
        Some(comm(&v))
    }
}

impl Host for Kernel {
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d); // waits-on: nothing — the drain period is the method
    }

    fn release(&mut self) -> Option<String> {
        std::fs::read_link(&self.release_root).ok().map(|p| p.display().to_string())
    }

    fn drain(&mut self) -> Result<Drain, String> {
        self.walked = false;
        let mut d = Drain::default();
        let (starts, threads) = (fd_of(&self.maps[0]), fd_of(&self.maps[1]));
        for k in keys(starts, START_KEY) {
            if let Some(v) = lookup(starts, &k, 8, true) {
                d.starts.push(StartCount { cgroup: u64_at(&k, 0), program: comm(&k[8..8 + COMM]), starter: comm(&k[8 + COMM..8 + 2 * COMM]), n: u64_at(&v, 0) });
            }
        }
        d.threads = take_counts(threads);
        d.execs = take_counts(fd_of(&self.maps[6]));
        d.host_forks = std::fs::read_to_string(format!("{}/stat", system_paths::PROC)).ok().and_then(|t| host_forks(&t));
        for (i, slot) in d.dropped.iter_mut().enumerate() {
            *slot = lookup(fd_of(&self.maps[3]), &(i as u32).to_ne_bytes(), 8, false).map_or(0, |v| u64_at(&v, 0));
        }
        for (i, slot) in d.cost.iter_mut().enumerate() {
            let per_cpu = lookup(fd_of(&self.maps[4]), &(i as u32).to_ne_bytes(), 16 * self.cpus, false).unwrap_or_default();
            *slot = per_cpu.as_chunks::<16>().0.iter().fold((0, 0), |(ns, runs), c| (ns + u64_at(c, 0), runs + u64_at(c, 8)));
        }
        Ok(d)
    }

    fn resolve(&mut self, cgroup: u64) -> Option<String> {
        if let Some(p) = self.cache.get(&cgroup) {
            return Some(p.clone());
        }
        if !self.walked {
            self.walked = true;
            if self.cache.len() > CACHE_MAX {
                self.cache.clear();
            }
            self.walk(Path::new(system_paths::SYS_FS_CGROUP), "");
        }
        self.cache.get(&cgroup).cloned().or_else(|| self.named(cgroup))
    }

    fn self_cpu_seconds(&mut self) -> f64 {
        std::fs::read_to_string(format!("{}/self/stat", system_paths::PROC)).ok().and_then(|t| cpu_seconds(&t, self.clk_tck)).unwrap_or(0.0)
    }

    fn push(&mut self, series: &[Series]) -> Result<(), String> {
        crate::sink().try_push_series(series).map_err(|e| e.to_string())
    }
}

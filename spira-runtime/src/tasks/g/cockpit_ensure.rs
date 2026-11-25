//! `cockpit.ensure`: starts the installed `cockpit-ensure.service` (`layout ensure`) only when
//! an input of the cockpit moved. It never judges cockpit health; that stays in `Layout::ensure`,
//! which it asks by starting the unit.
//!
//! Observe reads `/proc` and a few files and starts no program. Decide starts the unit on the
//! first tick, when the observation differs from the one the last start ran against, or when the
//! last start is older than `COCKPIT_CLIENT_IDLE_SECS` (idle-client detach). The carry is that
//! observation and the time of the start, recorded when the start is asked, so a failed run is
//! not retried until an input moves; the red stays on the unit and in `run/cockpit-heal.log`.

use std::path::Path;
use std::time::UNIX_EPOCH;

use runtime::event::*;
use runtime::units::{UnitHandle, UNIT_START_INSTALLED};
use serde::{Deserialize, Serialize};

use crate::tasks::d::pattern;
use crate::watchers::carried;

pub const NAME: &str = "cockpit.ensure";
pub const STREAM: &str = "cockpit.ensure";
pub const TICK: EventKindTag = "cockpit.ensure.tick";
pub const STATE: EventKindTag = "cockpit.ensure.state";
pub const OBSERVE: &str = "cockpit.ensure.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, UNIT_START_INSTALLED];
pub const KEYS: &[&str] = &[];

/// The unit `layout ensure` runs in; `install::manifest::build` ships it.
pub const UNIT: &str = "cockpit-ensure.service";
pub const CADENCE_MS: u64 = 60_000;
pub const BOOT_DELAY_MS: u64 = 120_000;
pub const OBSERVE_DEADLINE_MS: u64 = 5_000;
pub const START_DEADLINE_MS: u64 = 10_000;
const BOOT: &str = "boot";
const SERVER: &str = "tmux: server";
const CLIENT: &str = "tmux: client";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proc {
    pub pid: u32,
    pub started: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conf {
    pub right_pct: u32,
    pub bottom_pct: u32,
    pub mail: Option<String>,
    pub mouse: bool,
    pub clipboard: bool,
    pub idle_secs: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub servers: Vec<Proc>,
    pub children: Vec<Proc>,
    pub clients: Vec<Proc>,
    pub down: bool,
    pub health_mtime_ns: Option<u64>,
    pub layout_mtime_ns: Option<u64>,
    pub conf: Conf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub held: Option<Observation>,
    pub started_ms: u64,
}

/// The pure decision: the unit starts on the first tick, on a moved observation, or when the
/// held start is older than the idle-client limit (never when it is 0).
pub fn decide(carry: &Carry, obs: &Observation, now_ms: u64) -> (Carry, Vec<EffectSpec>) {
    let idle_ms = u64::try_from(obs.conf.idle_secs).unwrap_or(0).saturating_mul(1000);
    let due = match &carry.held {
        None => true,
        Some(held) => held != obs || (idle_ms > 0 && now_ms.saturating_sub(carry.started_ms) >= idle_ms),
    };
    if !due {
        return (carry.clone(), vec![]);
    }
    let input = serde_json::to_string(&UnitHandle::named(UNIT)).unwrap_or_default();
    (Carry { held: Some(obs.clone()), started_ms: now_ms }, vec![EffectSpec::keyed(UNIT_START_INSTALLED, format!("{NAME}:{now_ms}"), input, START_DEADLINE_MS)])
}

struct Stat {
    pid: u32,
    comm: String,
    ppid: u32,
    started: u64,
}

/// One `stat` text; `comm` may hold spaces and parens.
fn parse_stat(pid: u32, text: &str) -> Option<Stat> {
    let (open, close) = (text.find('(')?, text.rfind(')')?);
    let comm = text.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    Some(Stat { pid, comm, ppid: rest.get(1)?.parse().ok()?, started: rest.get(19)?.parse().ok()? })
}

fn mtime_ns(path: &Path) -> Option<u64> {
    let d = std::fs::metadata(path).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(d.as_nanos()).ok()
}

/// The tmux servers, the direct children of those, and the tmux clients, each by pid.
pub type Procs = (Vec<Proc>, Vec<Proc>, Vec<Proc>);

/// The tmux processes under `proc_root` by their `comm` and parent, never by argv.
pub fn scan_proc(proc_root: &Path) -> Result<Procs, String> {
    let entries = std::fs::read_dir(proc_root).map_err(|e| format!("{}: {e}", proc_root.display()))?;
    let all: Vec<Stat> = entries
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            parse_stat(pid, &std::fs::read_to_string(e.path().join("stat")).ok()?)
        })
        .collect();
    let of = |keep: &dyn Fn(&Stat) -> bool| -> Vec<Proc> {
        let mut v: Vec<Proc> = all.iter().filter(|p| keep(p)).map(|p| Proc { pid: p.pid, started: p.started }).collect();
        v.sort_by_key(|p| p.pid);
        v
    };
    let servers: Vec<u32> = all.iter().filter(|p| p.comm == SERVER).map(|p| p.pid).collect();
    Ok((of(&|p| p.comm == SERVER), of(&|p| servers.contains(&p.ppid)), of(&|p| p.comm == CLIENT)))
}

pub fn observe_at(proc_root: &Path, conf: &cockpit_ops::layout::Conf) -> Result<Observation, String> {
    let (servers, children, clients) = scan_proc(proc_root)?;
    let bin = Path::new(&conf.spira_release).join("bin");
    Ok(Observation {
        servers,
        children,
        clients,
        down: conf.down_marker().exists(),
        health_mtime_ns: mtime_ns(&bin.join("health")),
        layout_mtime_ns: mtime_ns(&bin.join("layout")),
        conf: Conf { right_pct: conf.right_pct, bottom_pct: conf.bottom_pct, mail: conf.mail_cmd.clone(), mouse: conf.mouse_on, clipboard: conf.clipboard_on, idle_secs: conf.idle_secs },
    })
}

/// The observe runner: `proc_root` (the settings' `/proc`) and the cockpit's own conf, as `layout` resolves it.
pub fn observe(proc_root: &Path) -> Result<String, String> {
    let obs = observe_at(proc_root, &cockpit_ops::layout::Conf::from_env()?)?;
    serde_json::to_string(&obs).map_err(|e| e.to_string())
}

pub struct CockpitEnsure;

impl CockpitEnsure {
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cockpit.ensure/boot", STREAM, EventKind::new(TICK, BOOT))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(RowCodec { stream: STREAM, also: &[], tag: STATE, split: runtime::rows::split_whole, join: runtime::rows::join_whole })
    }
}

impl Handler for CockpitEnsure {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK && ev.kind.body == BOOT {
            return vec![Command::Timer { at: ev.at + BOOT_DELAY_MS, kind: EventKind::new(TICK, "") }];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        let Some(obs) = pattern::answer::<Observation>(view, ev, OBSERVE) else { return vec![] };
        let _ = ctx;
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, acts) = decide(&carry, &obs, ev.at);
        pattern::commands(STATE, &carry, &next, acts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn obs() -> Observation {
        Observation {
            servers: vec![Proc { pid: 10, started: 100 }],
            children: vec![Proc { pid: 11, started: 101 }, Proc { pid: 12, started: 102 }],
            clients: vec![Proc { pid: 20, started: 200 }],
            down: false,
            health_mtime_ns: Some(5),
            layout_mtime_ns: Some(6),
            conf: Conf { right_pct: 33, bottom_pct: 28, mail: Some("mutt".into()), mouse: true, clipboard: true, idle_secs: 21_600 },
        }
    }

    fn held(o: &Observation) -> Carry {
        Carry { held: Some(o.clone()), started_ms: NOW }
    }

    fn starts(carry: &Carry, o: &Observation, now: u64) -> usize {
        decide(carry, o, now).1.len()
    }

    #[test_id(tq1pjzg)]
    #[test]
    fn the_first_tick_starts_the_unit_once_and_names_it() {
        let (next, acts) = decide(&Carry::default(), &obs(), NOW);
        assert_eq!(acts.len(), 1);
        assert_eq!((acts[0].name.as_str(), acts[0].input.as_str()), (UNIT_START_INSTALLED, serde_json::to_string(&UnitHandle::named(UNIT)).unwrap().as_str()));
        assert_eq!(next, held(&obs()));
    }

    #[test_id(t1gtw9l)]
    #[test]
    fn an_equal_observation_starts_nothing_and_leaves_the_carry_alone() {
        let c = held(&obs());
        let (next, acts) = decide(&c, &obs(), NOW + 3_600_000);
        assert!(acts.is_empty());
        assert_eq!(next, c);
    }

    #[test_id(t7gc5mx)]
    #[test]
    fn each_moved_input_starts_the_unit_once_and_is_then_held() {
        let base = obs();
        type Move = (&'static str, fn(&mut Observation));
        let moved: [Move; 11] = [
            ("a server child gone", |o| {
                o.children.pop();
            }),
            ("a new child", |o| o.children.push(Proc { pid: 13, started: 103 })),
            ("a new client", |o| o.clients.push(Proc { pid: 21, started: 210 })),
            ("the server restarted", |o| o.servers[0].started += 1),
            ("down marker added", |o| o.down = true),
            ("health mtime", |o| o.health_mtime_ns = Some(7)),
            ("layout mtime", |o| o.layout_mtime_ns = Some(8)),
            ("layout binary removed", |o| o.layout_mtime_ns = None),
            ("right pct", |o| o.conf.right_pct = 40),
            ("mail", |o| o.conf.mail = None),
            ("mouse", |o| o.conf.mouse = false),
        ];
        for (what, change) in moved {
            let mut o = base.clone();
            change(&mut o);
            let (next, acts) = decide(&held(&base), &o, NOW + 60_000);
            assert_eq!(acts.len(), 1, "{what}");
            assert_eq!(starts(&next, &o, NOW + 120_000), 0, "{what}: held after its start");
        }
        let mut down = base.clone();
        down.down = true;
        assert_eq!(starts(&held(&down), &base, NOW + 60_000), 1, "down marker removed");
    }

    #[test_id(txucole)]
    #[test]
    fn a_start_older_than_the_client_idle_limit_starts_once_and_never_when_the_limit_is_zero() {
        let o = obs();
        let limit_ms = o.conf.idle_secs as u64 * 1000;
        assert_eq!(starts(&held(&o), &o, NOW + limit_ms - 1), 0);
        let (next, acts) = decide(&held(&o), &o, NOW + limit_ms);
        assert_eq!(acts.len(), 1);
        assert_eq!(starts(&next, &o, NOW + limit_ms + 60_000), 0);
        let mut off = o.clone();
        off.conf.idle_secs = 0;
        assert_eq!(starts(&held(&off), &off, NOW + 10 * limit_ms), 0);
    }

    #[test_id(tlr5ye8)]
    #[test]
    fn a_start_is_recorded_under_the_observation_it_ran_against_so_a_red_is_not_retried() {
        let mut o = obs();
        o.down = true;
        let (next, acts) = decide(&held(&obs()), &o, NOW + 60_000);
        assert_eq!(acts.len(), 1);
        assert_eq!(next.held.as_ref(), Some(&o));
        assert_eq!(starts(&next, &o, NOW + 120_000), 0, "the unit's failure is not an input");
    }

    fn stat(comm: &str, ppid: u32, started: u64) -> String {
        let tail = ["0"; 2].join(" ");
        format!("1 ({comm}) S {ppid} 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 {tail} {started} 0\n")
    }

    fn put(root: &Path, pid: u32, comm: &str, ppid: u32, started: u64) {
        std::fs::create_dir_all(root.join(pid.to_string())).unwrap();
        std::fs::write(root.join(pid.to_string()).join("stat"), stat(comm, ppid, started)).unwrap();
        std::fs::write(root.join(pid.to_string()).join("cmdline"), format!("{comm}\0tmux\0")).unwrap();
    }

    #[test_id(tggvnqv)]
    #[test]
    fn a_tmux_server_its_children_and_a_client_read_back_and_an_argv_that_says_tmux_is_not_a_server() {
        let dir = testkit::TempDir::new("cockpit-ensure-proc");
        put(dir.path(), 100, "tmux: server", 1, 5000);
        put(dir.path(), 101, "bash", 100, 5001);
        put(dir.path(), 102, "health", 100, 5002);
        put(dir.path(), 200, "tmux: client", 50, 6000);
        put(dir.path(), 300, "vim", 1, 7000);
        put(dir.path(), 301, "sh", 300, 7001);
        std::fs::create_dir_all(dir.join("self")).unwrap();
        let (servers, children, clients) = scan_proc(dir.path()).unwrap();
        assert_eq!(servers, vec![Proc { pid: 100, started: 5000 }]);
        assert_eq!(children, vec![Proc { pid: 101, started: 5001 }, Proc { pid: 102, started: 5002 }]);
        assert_eq!(clients, vec![Proc { pid: 200, started: 6000 }]);
    }

    #[test_id(t3l9jug)]
    #[test]
    fn a_comm_holding_spaces_and_parens_does_not_shift_the_fields() {
        let got = parse_stat(7, &stat("a) (b c", 4, 99)).unwrap();
        assert_eq!((got.pid, got.comm.as_str(), got.ppid, got.started), (7, "a) (b c", 4, 99));
    }

    #[test_id(trz0d8o)]
    #[test]
    fn the_unit_it_starts_is_one_the_installer_ships() {
        let m = install::manifest::build(&install::manifest::Inputs::default()).unwrap();
        assert!(m.units.iter().any(|u| u.name == UNIT), "{UNIT} is not in the manifest");
    }
}

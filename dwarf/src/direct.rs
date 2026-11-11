//! direct — a dwarf of one rune on one named bead, outside the caps and the world's state.
//! `dwarf direct <rune> <bead> --by <who> --reason <why>`.
//!
//! The dwarf is the sentinel's own (same rune prompt, hooks, fences, build-dir lease, submit
//! and session end): this starts it by the same transient-unit call, and only the claim differs.
//! The unit is named `spira-dwarf-<rune>-directed-<epoch>` so drain, slay and the cockpit see a
//! dwarf while no cap counts it (`runtime::units::counts_toward_caps`). The claim is the
//! ordinary compare-and-set, so a second direct of the same bead loses it: one directed dwarf
//! per bead. A bead a dwarf holds is taken over: that dwarf is stopped with its work kept, then
//! the bead is claimed on the same branch.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ports::{Exec, s};
use crate::util::{self, Sink};

/// Who directed a dwarf, why, and which dwarf held the bead before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Direct {
    pub bead: String,
    pub by: String,
    pub reason: String,
    pub replaced: Option<String>,
}

impl Direct {
    /// The record the Claim event carries (`lifecycle::bead::Direction`).
    pub fn claim_json(&self) -> String {
        serde_json::json!({ "by": self.by, "reason": self.reason, "previous_holder": self.replaced }).to_string()
    }
}

/// What the Concierge typed: `direct <rune> <bead> --by <who> --reason <why>`, without `direct`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub rune: String,
    pub bead: String,
    pub by: String,
    pub reason: String,
}

const USAGE: &str = "usage: dwarf direct <rune> <bead-id> --by <who> --reason <why>";

fn flags(rest: &[String], allowed: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut it = rest.iter();
    while let Some(f) = it.next() {
        if !allowed.contains(&f.as_str()) {
            return Err(format!("{USAGE}\nunexpected argument {f:?}"));
        }
        let v = it.next().filter(|v| !v.is_empty()).ok_or_else(|| format!("{USAGE}\n{f} needs a value"))?;
        out.push((f.clone(), v.clone()));
    }
    Ok(out)
}

fn flag(found: &[(String, String)], name: &str) -> Option<String> {
    found.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

pub fn parse_request(args: &[String]) -> Result<Request, String> {
    let (rune, bead) = match (args.first(), args.get(1)) {
        (Some(r), Some(b)) if !r.starts_with('-') && !b.starts_with('-') && !r.is_empty() && !b.is_empty() => (r.clone(), b.clone()),
        _ => return Err(USAGE.to_string()),
    };
    let f = flags(&args[2..], &["--by", "--reason"])?;
    let by = flag(&f, "--by").ok_or_else(|| format!("{USAGE}\n--by is required"))?;
    let reason = flag(&f, "--reason").ok_or_else(|| format!("{USAGE}\n--reason is required"))?;
    Ok(Request { rune, bead, by, reason })
}

/// The unit's own argv tail, after the dwarf path: `--home H <rune> --directed <bead> …`.
pub fn parse_directed(rest: &[String]) -> Result<Direct, String> {
    let usage = "usage: dwarf <rune> --directed <bead-id> --by <who> --reason <why> [--replaced <holder>]";
    let f = flags(rest, &["--directed", "--by", "--reason", "--replaced"]).map_err(|e| e.replace(USAGE, usage))?;
    let need = |k: &str| flag(&f, k).ok_or_else(|| format!("{usage}\n{k} is required"));
    Ok(Direct { bead: need("--directed")?, by: need("--by")?, reason: need("--reason")?, replaced: flag(&f, "--replaced") })
}

/// The unit call: the dwarf is started as `<rune> --directed <bead> --by <who> --reason <why>
/// [--replaced <holder>]`.
pub fn build_argv(req: &Request, replaced: Option<&str>, home: &Path, dwarf_path: &str, call_argv: &[String], epoch: i64) -> Vec<String> {
    let mut tail = s(&["--directed", &req.bead, "--by", &req.by, "--reason", &req.reason]);
    if let Some(h) = replaced {
        tail.extend(s(&["--replaced", h]));
    }
    crate::escape::unit_call_argv(&runtime::units::directed_unit_stem(&req.rune, epoch), home, dwarf_path, call_argv, &req.rune, &tail)
}

/// The reason `slay` records when a directs takes a bead from a running dwarf.
pub fn takeover_why(req: &Request) -> String {
    format!("directed by {}: {}", req.by, req.reason)
}

/// States a claim can apply to, and the one a takeover releases from.
const CLAIMABLE: [&str; 2] = ["READY", "REWORK"];
const HELD: &str = "WORKING";

/// `spira-lc show <bead>`: the row's state and holder; `None` when the machine has no row yet
/// (the claim creates it), an error when it could not answer.
fn row_of(exec: &dyn Exec, bead: &str) -> Result<Option<(String, Option<String>)>, String> {
    let o = exec.exec("spira-lc", &s(&["show", bead]), None, None);
    match o.code {
        1 => Ok(None),
        0 => spira_config::lc_state::parse_show(&o.stdout).map(|r| r.map(|r| (r.state, r.holder))),
        rc => Err(format!("spira-lc show {bead} failed (rc={rc}): {}", o.first_err_line())),
    }
}

/// How long the launcher waits on the called dwarf, and for a stopped dwarf's row to settle.
const WAIT_TICKS: u32 = 60;
const TICK: Duration = Duration::from_secs(1);

fn unit_stem(unit: &str) -> &str {
    unit.strip_suffix(".service").unwrap_or(unit)
}

/// Where a called dwarf leaves its refusal for the launcher, which cannot see the unit's output.
pub fn refusal_path(run: &Path, unit: &str) -> PathBuf {
    run.join("direct-refused").join(unit_stem(unit))
}

/// Records why this directed dwarf turned the bead away. Best effort: the ledger and stderr
/// carry the same reason.
pub fn write_refusal(run: &Path, unit: &str, reason: &str) {
    if unit.is_empty() {
        return;
    }
    let path = refusal_path(run, unit);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{reason}\n"));
}

/// The refusal in words: the row the dwarf saw, and what each claim attempt said.
pub fn refusal_reason(bead: &str, by: &str, row: &Result<Option<spira_config::lc_state::Row>, String>, attempts: &[String]) -> String {
    let seen = match row {
        Ok(Some(r)) => format!("it reads state={} holder={} holds=[{}]", r.state, r.holder.as_deref().unwrap_or("none"), r.holds.join(",")),
        Ok(None) => "it has no lifecycle row".to_string(),
        Err(e) => format!("its row could not be read ({e})"),
    };
    let said = if attempts.is_empty() { String::new() } else { format!("; the claim said: {}", attempts.join(" | ")) };
    format!("{bead} was not claimable for {by} — {seen}{said}")
}

#[allow(clippy::too_many_arguments)]
pub fn run(exec: &dyn Exec, sink: &dyn Sink, sleep: &dyn Fn(Duration), run_dir: &Path, req: &Request, home: &Path, dwarf_path: Option<&str>, call_bin: &str, call_argv: &[String], now: i64) -> i32 {
    let fatal = |m: String| {
        sink.err(&util::log_line(now, &format!("FATAL direct {}: {m}", req.bead)));
        1
    };
    let Some(dwarf_path) = dwarf_path else { return fatal("dwarf is not on PATH".into()) };
    let row = match row_of(exec, &req.bead) {
        Ok(r) => r,
        Err(e) => return fatal(format!("{e} — nothing directed")),
    };
    let mut replaced = None;
    if let Some((state, holder)) = row {
        if state == HELD {
            let o = exec.exec("slay", &s(&["--bead", &req.bead, "--keep-work", "--why", &takeover_why(req)]), None, None);
            if !o.success() {
                return fatal(format!("could not stop the dwarf holding it (slay rc={}): {} — nothing directed", o.code, o.first_err_line()));
            }
            let _ = exec.exec("spira-lc", &s(&["unhold", &req.bead, "wait", "dwarf"]), None, None);
            let mut seen = String::new();
            let settled = (0..WAIT_TICKS).any(|_| match row_of(exec, &req.bead) {
                Ok(Some((state, _))) if CLAIMABLE.contains(&state.as_str()) => true,
                other => {
                    seen = format!("{other:?}");
                    sleep(TICK);
                    false
                }
            });
            if !settled {
                return fatal(format!("the stopped dwarf's row never left WORKING (last read {seen}) — nothing directed"));
            }
            sink.out(&util::log_line(now, &format!("direct {}: stopped {} with its branch and worktree kept", req.bead, holder.as_deref().unwrap_or("its holder"))));
            replaced = Some(holder.unwrap_or_else(|| "unknown".to_string()));
        } else if !CLAIMABLE.contains(&state.as_str()) {
            return fatal(format!("it is {state}; only READY, REWORK or a held WORKING bead can be directed onto — nothing directed"));
        }
    }
    let argv = build_argv(req, replaced.as_deref(), home, dwarf_path, call_argv, now);
    sink.out(&util::log_line(now, &format!("direct {}: calling a {} dwarf directly (caps and the world's state bypassed)", req.bead, req.rune)));
    let o = exec.exec(call_bin, &argv, None, None);
    o.stdout.lines().for_each(|l| sink.out(l));
    o.stderr.lines().for_each(|l| sink.err(l));
    if o.code != 0 {
        return o.code;
    }
    let refusal = refusal_path(run_dir, &runtime::units::directed_unit_stem(&req.rune, now));
    for _ in 0..WAIT_TICKS {
        if let Ok(why) = std::fs::read_to_string(&refusal) {
            let _ = std::fs::remove_file(&refusal);
            return fatal(format!("the dwarf refused it: {}", why.trim_end()));
        }
        if let Ok(Some((state, Some(holder)))) = row_of(exec, &req.bead) {
            if state == HELD && replaced.as_deref() != Some(holder.as_str()) {
                return 0;
            }
        }
        sleep(TICK);
    }
    sink.out(&util::log_line(now, &format!("direct {}: the dwarf has neither claimed nor refused it after {}s — see its unit", req.bead, WAIT_TICKS)));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{MemSink, Out};
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn a(v: &[&str]) -> Vec<String> {
        s(v)
    }

    fn req() -> Request {
        Request { rune: "builder".into(), bead: "sp-x".into(), by: "concierge".into(), reason: "loop fix".into() }
    }

    #[test]
    fn the_request_names_rune_bead_requester_and_reason() {
        assert_eq!(parse_request(&a(&["builder", "sp-x", "--by", "concierge", "--reason", "loop fix"])), Ok(req()));
        for bad in [&["builder"][..], &["builder", "sp-x"], &["builder", "sp-x", "--by", "c"], &["builder", "sp-x", "--reason", "r"], &["--by", "c", "builder", "sp-x"], &["builder", "sp-x", "--by", "c", "--reason", "r", "--stray", "z"]] {
            assert!(parse_request(&a(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_unit_tail_round_trips_what_the_caller_built() {
        let argv = build_argv(&req(), Some("dwarf-old"), Path::new("/h"), "/rel/bin/dwarf", &a(&["--setenv=HOME=/h"]), 1000);
        assert_eq!(&argv[..4], &a(&["--user", "--collect", "--quiet", "--unit=spira-dwarf-builder-directed-1000"]));
        let tail = &argv[argv.iter().position(|x| x == "--directed").unwrap()..];
        assert_eq!(parse_directed(tail), Ok(Direct { bead: "sp-x".into(), by: "concierge".into(), reason: "loop fix".into(), replaced: Some("dwarf-old".into()) }));
        let at = argv.iter().position(|x| x == "/rel/bin/dwarf").unwrap();
        assert_eq!(argv[at..at + 4], a(&["/rel/bin/dwarf", "--home", "/h", "builder"])[..]);
        assert!(runtime::units::is_directed(&format!("{}.service", &argv[3]["--unit=".len()..])));
    }

    #[test]
    fn the_claim_record_names_requester_reason_and_previous_holder() {
        let s = Direct { bead: "sp-x".into(), by: "concierge".into(), reason: "a \"quoted\" why".into(), replaced: None };
        let v: serde_json::Value = serde_json::from_str(&s.claim_json()).unwrap();
        assert_eq!((v["by"].as_str(), v["reason"].as_str(), v["previous_holder"].is_null()), (Some("concierge"), Some("a \"quoted\" why"), true));
        let mut keys: Vec<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["by", "previous_holder", "reason"], "the keys lifecycle::bead::Direction reads");
    }

    struct Fake {
        show: Out,
        slay_rc: i32,
        calls: Mutex<Vec<(String, Vec<String>)>>,
        /// Reads of the row that still see the stopped dwarf holding it, after `slay`.
        teardown_reads: Mutex<u32>,
        /// What the row reads once the unit is launched; and what the unit leaves behind.
        after_launch: Option<Out>,
        refusal: Option<String>,
        run_dir: PathBuf,
    }
    impl Exec for Fake {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            let launched = self.calls.lock().unwrap().iter().any(|c| c.0 == "systemd-run");
            let slain = self.calls.lock().unwrap().iter().any(|c| c.0 == "slay");
            self.calls.lock().unwrap().push((prog.to_string(), args.to_vec()));
            match prog {
                "spira-lc" if launched && self.after_launch.is_some() => self.after_launch.clone().unwrap(),
                "spira-lc" if slain && args[0] == "show" => {
                    let mut n = self.teardown_reads.lock().unwrap();
                    if *n > 0 {
                        *n -= 1;
                        return self.show.clone();
                    }
                    row("READY", None)
                }
                "spira-lc" => self.show.clone(),
                "slay" => Out { code: self.slay_rc, stdout: String::new(), stderr: "slay said no".into() },
                "systemd-run" => {
                    if let Some(why) = &self.refusal {
                        write_refusal(&self.run_dir, "spira-dwarf-builder-directed-1000.service", why);
                    }
                    Out::ok("")
                }
                _ => Out::ok(""),
            }
        }
    }
    fn fake(show: Out, slay_rc: i32) -> Fake {
        Fake { show, slay_rc, calls: Mutex::default(), teardown_reads: Mutex::new(0), after_launch: None, refusal: None, run_dir: PathBuf::from("/nonexistent") }
    }
    fn row(state: &str, holder: Option<&str>) -> Out {
        let h = holder.map_or("null".to_string(), |h| format!("\"{h}\""));
        Out::ok(format!(r#"{{"bead":{{"bead_id":"sp-x","state":"{state}","holder":{h},"lease_until":null,"holds":"","express":0,"stack":"{{}}"}}}}"#))
    }
    fn go(f: &Fake) -> (i32, MemSink) {
        let sink = MemSink::default();
        let rc = run(f, &sink, &|_| {}, &f.run_dir, &req(), Path::new("/h"), Some("/rel/bin/dwarf"), "systemd-run", &[], 1000);
        (rc, sink)
    }
    fn progs(f: &Fake) -> Vec<String> {
        let mut p: Vec<String> = f.calls.lock().unwrap().iter().map(|c| c.0.clone()).collect();
        p.dedup();
        p
    }

    #[test]
    fn a_ready_bead_is_called_without_a_takeover() {
        let f = { let mut f = fake(row("READY", None), 0); f.after_launch = Some(row("WORKING", Some("dwarf-new"))); f };
        let (rc, _) = go(&f);
        assert_eq!((rc, progs(&f)), (0, a(&["spira-lc", "systemd-run", "spira-lc"])));
    }

    #[test]
    fn a_held_bead_is_taken_over_with_its_work_kept_and_the_requester_in_the_why() {
        let f = { let mut f = fake(row("WORKING", Some("dwarf-old")), 0); f.after_launch = Some(row("WORKING", Some("dwarf-new"))); f };
        let (rc, sink) = go(&f);
        assert_eq!(rc, 0);
        assert_eq!(progs(&f), a(&["spira-lc", "slay", "spira-lc", "systemd-run", "spira-lc"]), "the old dwarf stops and its leftover hold is lifted before the new one starts");
        let calls = f.calls.lock().unwrap();
        let at = |p: &str| calls.iter().find(|c| c.0 == p && c.1[0] != "show").unwrap().1.clone();
        assert_eq!(at("spira-lc"), a(&["unhold", "sp-x", "wait", "dwarf"]));
        assert_eq!(at("slay"), a(&["--bead", "sp-x", "--keep-work", "--why", "directed by concierge: loop fix"]));
        assert!(at("systemd-run").windows(2).any(|w| w == ["--replaced", "dwarf-old"]), "{:?}", at("systemd-run"));
        assert!(sink.all().contains("stopped dwarf-old"));
    }

    #[test]
    fn a_dwarf_that_will_not_stop_means_nothing_is_directed() {
        let f = fake(row("WORKING", Some("dwarf-old")), 1);
        let (rc, sink) = go(&f);
        assert_eq!((rc, progs(&f)), (1, a(&["spira-lc", "slay"])));
        assert!(sink.all().contains("slay said no"), "{}", sink.all());
    }

    #[test]
    fn a_bead_past_the_builder_or_an_unreadable_row_is_refused_before_anything_runs() {
        let f = fake(row("SUBMITTED", None), 0);
        assert_eq!((go(&f).0, progs(&f)), (1, a(&["spira-lc"])));
        let f = fake(Out { code: 2, stdout: String::new(), stderr: "store down".into() }, 0);
        assert_eq!((go(&f).0, progs(&f)), (1, a(&["spira-lc"])));
        let f = fake(Out { code: 1, stdout: String::new(), stderr: String::new() }, 0);
        assert_eq!((go(&f).0, progs(&f)), (0, a(&["spira-lc", "systemd-run", "spira-lc"])), "no row yet: the claim creates it");
    }

    #[test]
    fn a_dwarf_that_refuses_the_bead_fails_the_direct_call_with_its_reason() {
        let dir = testkit::TempDir::new("dwarf-direct-refused");
        let mut f = fake(row("READY", None), 0);
        f.run_dir = dir.to_path_buf();
        f.refusal = Some("sp-x was not claimable for concierge — it reads state=READY holder=none holds=[]; the claim said: Blocked by parent sp-p".into());
        let (rc, sink) = go(&f);
        assert_eq!(rc, 1);
        assert!(sink.all().contains("ERR") && sink.all().contains("Blocked by parent sp-p") && sink.all().contains("state=READY"), "{}", sink.all());
    }

    #[test]
    fn a_takeover_waits_for_the_stopped_dwarfs_row_before_calling_the_next() {
        let mut f = fake(row("WORKING", Some("dwarf-old")), 0);
        f.teardown_reads = Mutex::new(3);
        f.after_launch = Some(row("WORKING", Some("dwarf-new")));
        let (rc, _) = go(&f);
        assert_eq!(rc, 0);
        let calls = f.calls.lock().unwrap();
        let launch = calls.iter().position(|c| c.0 == "systemd-run").unwrap();
        let slay = calls.iter().position(|c| c.0 == "slay").unwrap();
        let shows = calls[slay..launch].iter().filter(|c| c.0 == "spira-lc" && c.1[0] == "show").count();
        assert_eq!(shows, 4, "three reads still saw the old holder, the fourth saw READY");
    }

    #[test]
    fn a_row_that_never_leaves_working_after_the_stop_means_nothing_is_directed() {
        let mut f = fake(row("WORKING", Some("dwarf-old")), 0);
        f.teardown_reads = Mutex::new(u32::MAX);
        let (rc, sink) = go(&f);
        assert_eq!((rc, progs(&f).contains(&"systemd-run".to_string())), (1, false));
        assert!(sink.all().contains("never left WORKING"), "{}", sink.all());
    }

    #[test]
    fn the_refusal_names_the_row_it_saw_and_what_the_claim_said() {
        let r = spira_config::lc_state::parse_show(&row("READY", None).stdout);
        let why = refusal_reason("sp-x", "concierge", &r, &["lost to another claim".into()]);
        assert!(why.contains("sp-x") && why.contains("concierge") && why.contains("state=READY holder=none") && why.contains("lost to another claim"), "{why}");
        assert!(refusal_reason("sp-x", "c", &Ok(None), &[]).contains("no lifecycle row"));
    }
}

//! direct — a dwarf of one rune on one named bead, past the caps and the world's state.
//! `dwarf direct <rune> <bead> --by <who> --reason <why>`.
//!
//! The dwarf is the sentinel's own (same rune prompt, hooks, fences, build-dir lease, submit
//! and session end): this starts it by the same transient-unit call, and only the claim differs.
//! The unit is named `spira-dwarf-<rune>-directed-<epoch>` so drain, slay and the cockpit see a
//! dwarf; once live it holds a slot in the fleet ceiling and its rune's count, though starting it checks no cap. The claim is the
//! ordinary compare-and-set, so a second direct of the same bead loses it: one directed dwarf
//! per bead. A bead a dwarf holds is taken over: that dwarf is stopped with its work kept, then
//! the bead is claimed on the same branch.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ports::{Exec, s};
use crate::util::{self, Out, Sink};

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
        0 => spira_store::lc_state::parse_show(&o.stdout).map(|r| r.map(|r| (r.state, r.holder))),
        rc => Err(format!("spira-lc show {bead} failed (rc={rc}): {}", o.first_err_line())),
    }
}

/// How long the launcher waits on the called dwarf, and for a stopped dwarf's row to settle.
const WAIT_TICKS: u32 = 60;
const TICK: Duration = Duration::from_secs(1);

/// Where a called dwarf leaves its refusal for the launcher, which cannot see the unit's output.
/// One directed dwarf per bead, so the bead names it.
pub fn refusal_path(run: &Path, bead: &str) -> PathBuf {
    run.join("direct-refused").join(bead)
}

/// Where a directed dwarf's stdout and stderr go: the launcher reads its last line when the
/// dwarf neither claims nor refuses.
pub fn output_path(run: &Path, bead: &str) -> PathBuf {
    run.join("direct-out").join(format!("{bead}.log"))
}

/// The `systemd-run` flags of a directed dwarf's unit: its environment as `--setenv`, then its
/// properties.
pub fn call_flags(env_pairs: &[(String, String)], props: &runtime::units::UnitProps) -> Result<Vec<String>, String> {
    let mut flags: Vec<String> = env_pairs.iter().map(|(k, v)| format!("--setenv={k}={v}")).collect();
    flags.extend(props.run_flags().map_err(|e| e.to_string())?);
    Ok(flags)
}

/// What a dwarf that exited silently said last: its last line naming a FATAL, else its last
/// non-empty line.
fn last_said(output: &str) -> String {
    let lines: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines.iter().rev().find(|l| l.contains("FATAL")).or(lines.last()).map_or("the dwarf printed nothing".to_string(), |l| l.to_string())
}

/// Records why this directed dwarf turned the bead away. Best effort: the ledger and stderr
/// carry the same reason.
pub fn write_refusal(run: &Path, bead: &str, reason: &str) {
    let path = refusal_path(run, bead);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{reason}\n"));
}

/// The refusal in words: the row the dwarf saw, and what each claim attempt said.
pub fn refusal_reason(bead: &str, by: &str, row: &Result<Option<spira_store::lc_state::Row>, String>, attempts: &[String]) -> String {
    let seen = match row {
        Ok(Some(r)) => format!("it reads state={} holder={} holds=[{}]", r.state, r.holder.as_deref().unwrap_or("none"), r.holds.join(",")),
        Ok(None) => "it has no lifecycle row".to_string(),
        Err(e) => format!("its row could not be read ({e})"),
    };
    let said = if attempts.is_empty() { String::new() } else { format!("; the claim said: {}", attempts.join(" | ")) };
    format!("{bead} was not claimable for {by} — {seen}{said}")
}

/// slay's own reason: its stderr, else its stdout lines (slay reports a failed stop there).
fn slay_reason(o: &Out) -> String {
    let err = o.first_err_line();
    if !err.trim().is_empty() {
        return err;
    }
    let said: Vec<&str> = o.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if said.is_empty() { "slay gave no reason".to_string() } else { said.join("; ") }
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
                return fatal(format!("could not stop the dwarf holding it (slay rc={}): {} — nothing directed", o.code, slay_reason(&o)));
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
    let refusal = refusal_path(run_dir, &req.bead);
    let _ = std::fs::remove_file(&refusal);
    let output = output_path(run_dir, &req.bead);
    let _ = std::fs::write(&output, "");
    let o = exec.exec(call_bin, &argv, None, None);
    o.stdout.lines().for_each(|l| sink.out(l));
    o.stderr.lines().for_each(|l| sink.err(l));
    if o.code != 0 {
        return o.code;
    }
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
    let said = last_said(&std::fs::read_to_string(&output).unwrap_or_default());
    fatal(format!("the dwarf neither claimed nor refused it within {WAIT_TICKS}s — its last output ({}): {said}", output.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::MemSink;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn a(v: &[&str]) -> Vec<String> {
        s(v)
    }

    fn req() -> Request {
        Request { rune: "builder".into(), bead: "sp-x".into(), by: "concierge".into(), reason: "loop fix".into() }
    }

    #[test_id(tmxzld0)]
    #[test]
    fn the_request_names_rune_bead_requester_and_reason() {
        assert_eq!(parse_request(&a(&["builder", "sp-x", "--by", "concierge", "--reason", "loop fix"])), Ok(req()));
        for bad in [&["builder"][..], &["builder", "sp-x"], &["builder", "sp-x", "--by", "c"], &["builder", "sp-x", "--reason", "r"], &["--by", "c", "builder", "sp-x"], &["builder", "sp-x", "--by", "c", "--reason", "r", "--stray", "z"]] {
            assert!(parse_request(&a(bad)).is_err(), "{bad:?}");
        }
    }

    #[test_id(tif0iug)]
    #[test]
    fn the_unit_tail_round_trips_what_the_caller_built() {
        let argv = build_argv(&req(), Some("dwarf-old"), Path::new("h"), "rel/bin/dwarf", &a(&["--setenv=HOME=/h"]), 1000);
        assert_eq!(&argv[..4], &a(&["--user", "--collect", "--quiet", "--unit=spira-dwarf-builder-directed-1000"]));
        let tail = &argv[argv.iter().position(|x| x == "--directed").unwrap()..];
        assert_eq!(parse_directed(tail), Ok(Direct { bead: "sp-x".into(), by: "concierge".into(), reason: "loop fix".into(), replaced: Some("dwarf-old".into()) }));
        let at = argv.iter().position(|x| x == "rel/bin/dwarf").unwrap();
        assert_eq!(argv[at..at + 4], a(&["rel/bin/dwarf", "--home", "h", "builder"])[..]);
        assert!(runtime::units::is_directed(&format!("{}.service", &argv[3]["--unit=".len()..])));
    }

    #[test_id(th1kidm)]
    #[test]
    fn call_flags_carry_the_environment_and_the_dwarf_calls_properties_before_the_dwarf() {
        let env: Vec<(String, String)> = [("SPIRA_RELEASE", "r"), ("PATH", "r/bin"), ("HOME", "h"), ("SPIRA_TOML", "c.toml")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let props = runtime::units::UnitProps { output_append: Some(PathBuf::from("o/sp-x.log")), ..runtime::units::dwarf_props("3600").unwrap() };
        let flags = call_flags(&env, &props).unwrap();
        for want in ["--setenv=SPIRA_RELEASE=r", "--setenv=PATH=r/bin", "--setenv=HOME=h", "--setenv=SPIRA_TOML=c.toml", "--property=TimeoutStartSec=3600", "--property=StandardOutput=append:o/sp-x.log", "--property=StandardError=append:o/sp-x.log"] {
            assert!(flags.iter().any(|f| f == want), "{want}: {flags:?}");
        }
        let argv = build_argv(&req(), None, Path::new("h"), "rel/bin/dwarf", &flags, 1000);
        let dwarf = argv.iter().position(|x| x == "rel/bin/dwarf").unwrap();
        assert!(flags.iter().all(|f| argv.iter().position(|x| x == f).is_some_and(|i| i < dwarf)), "{argv:?}");
    }

    #[test_id(tjfapyb)]
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
        slay_out: (String, String),
        calls: Mutex<Vec<(String, Vec<String>)>>,
        /// Reads of the row that still see the stopped dwarf holding it, after `slay`.
        teardown_reads: Mutex<u32>,
        /// What the row reads once the unit is launched; and what the unit leaves behind.
        after_launch: Option<Out>,
        refusal: Option<String>,
        output: Option<String>,
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
                "slay" => Out { code: self.slay_rc, stdout: self.slay_out.0.clone(), stderr: self.slay_out.1.clone() },
                "systemd-run" => {
                    if let Some(why) = &self.refusal {
                        write_refusal(&self.run_dir, "sp-x", why);
                    }
                    if let Some(said) = &self.output {
                        std::fs::create_dir_all(output_path(&self.run_dir, "sp-x").parent().unwrap()).unwrap();
                        std::fs::write(output_path(&self.run_dir, "sp-x"), format!("{said}\n")).unwrap();
                    }
                    Out::ok("")
                }
                _ => Out::ok(""),
            }
        }
    }
    fn fake(show: Out, slay_rc: i32) -> Fake {
        Fake { show, slay_rc, slay_out: (String::new(), "slay said no".into()), calls: Mutex::default(), teardown_reads: Mutex::new(0), after_launch: None, refusal: None, output: None, run_dir: PathBuf::from("nonexistent") }
    }
    fn row(state: &str, holder: Option<&str>) -> Out {
        let h = holder.map_or("null".to_string(), |h| format!("\"{h}\""));
        Out::ok(format!(r#"{{"bead":{{"bead_id":"sp-x","state":"{state}","holder":{h},"lease_until":null,"holds":"","express":0,"stack":"{{}}"}}}}"#))
    }
    fn go(f: &Fake) -> (i32, MemSink) {
        let sink = MemSink::default();
        let rc = run(f, &sink, &|_| {}, &f.run_dir, &req(), Path::new("h"), Some("rel/bin/dwarf"), "systemd-run", &[], 1000);
        (rc, sink)
    }
    fn progs(f: &Fake) -> Vec<String> {
        let mut p: Vec<String> = f.calls.lock().unwrap().iter().map(|c| c.0.clone()).collect();
        p.dedup();
        p
    }

    #[test_id(t8hdypo)]
    #[test]
    fn a_ready_bead_is_called_without_a_takeover() {
        let f = { let mut f = fake(row("READY", None), 0); f.after_launch = Some(row("WORKING", Some("dwarf-new"))); f };
        let (rc, _) = go(&f);
        assert_eq!((rc, progs(&f)), (0, a(&["spira-lc", "systemd-run", "spira-lc"])));
    }

    #[test_id(tkaniys)]
    #[test]
    fn a_direct_past_a_full_fleet_still_builds_its_unit_call_and_asks_no_cap() {
        let f = { let mut f = fake(row("READY", None), 0); f.after_launch = Some(row("WORKING", Some("dwarf-new"))); f };
        let (rc, _) = go(&f);
        let calls = f.calls.lock().unwrap();
        let started = calls.iter().filter(|c| c.0 == "systemd-run").count();
        let asked_a_cap = calls.iter().any(|c| !matches!(c.0.as_str(), "spira-lc" | "systemd-run" | "slay"));
        assert_eq!((rc, started, asked_a_cap), (0, 1, false), "{calls:?}");
        let unit = calls.iter().find(|c| c.0 == "systemd-run").unwrap();
        assert!(unit.1.iter().any(|x| x.starts_with("--unit=spira-dwarf-builder-directed-")), "{unit:?}");
    }

    #[test_id(toyf53a)]
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

    #[test_id(t4oo6av)]
    #[test]
    fn a_slay_that_fails_on_stdout_alone_still_gives_its_reason() {
        let mut f = fake(row("WORKING", Some("dwarf-old")), 1);
        f.slay_out = ("verify: a pid file for sp-x remains\nslay: INCOMPLETE for sp-x — see above\n".into(), String::new());
        let (rc, sink) = go(&f);
        assert_eq!((rc, progs(&f)), (1, a(&["spira-lc", "slay"])));
        assert!(sink.all().contains("a pid file for sp-x remains"), "{}", sink.all());
    }

    #[test_id(toezrjl)]
    #[test]
    fn a_dwarf_that_will_not_stop_means_nothing_is_directed() {
        let f = fake(row("WORKING", Some("dwarf-old")), 1);
        let (rc, sink) = go(&f);
        assert_eq!((rc, progs(&f)), (1, a(&["spira-lc", "slay"])));
        assert!(sink.all().contains("slay said no"), "{}", sink.all());
    }

    #[test_id(ttjva7f)]
    #[test]
    fn a_bead_past_the_builder_or_an_unreadable_row_is_refused_before_anything_runs() {
        let f = fake(row("SUBMITTED", None), 0);
        assert_eq!((go(&f).0, progs(&f)), (1, a(&["spira-lc"])));
        let f = fake(Out { code: 2, stdout: String::new(), stderr: "store down".into() }, 0);
        assert_eq!((go(&f).0, progs(&f)), (1, a(&["spira-lc"])));
        let f = { let mut f = fake(Out { code: 1, stdout: String::new(), stderr: String::new() }, 0); f.after_launch = Some(row("WORKING", Some("dwarf-new"))); f };
        assert_eq!((go(&f).0, progs(&f)), (0, a(&["spira-lc", "systemd-run", "spira-lc"])), "no row yet: the claim creates it");
    }

    #[test_id(txzw2fr)]
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

    #[test_id(t4o64d5)]
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

    #[test_id(tcn5x9q)]
    #[test]
    fn a_row_that_never_leaves_working_after_the_stop_means_nothing_is_directed() {
        let mut f = fake(row("WORKING", Some("dwarf-old")), 0);
        f.teardown_reads = Mutex::new(u32::MAX);
        let (rc, sink) = go(&f);
        assert_eq!((rc, progs(&f).contains(&"systemd-run".to_string())), (1, false));
        assert!(sink.all().contains("never left WORKING"), "{}", sink.all());
    }

    #[test_id(tuaxv4x)]
    #[test]
    fn the_refusal_names_the_row_it_saw_and_what_the_claim_said() {
        let r = spira_store::lc_state::parse_show(&row("READY", None).stdout);
        let why = refusal_reason("sp-x", "concierge", &r, &["lost to another claim".into()]);
        assert!(why.contains("sp-x") && why.contains("concierge") && why.contains("state=READY holder=none") && why.contains("lost to another claim"), "{why}");
        assert!(refusal_reason("sp-x", "c", &Ok(None), &[]).contains("no lifecycle row"));
    }

    #[test_id(tb21ls8)]
    #[test]
    fn a_dwarf_that_neither_claims_nor_refuses_fails_the_call() {
        let dir = testkit::TempDir::new("dwarf-direct-silent");
        let said = "FATAL dwarf: neither SPIRA_HOME nor SPIRA_RELEASE is set — refusing to search for the harness home";
        let f = { let mut f = fake(row("READY", None), 0); f.run_dir = dir.to_path_buf(); f.output = Some(said.into()); f };
        let (rc, sink) = go(&f);
        assert_eq!(rc, 1);
        let all = sink.all();
        assert!(all.contains("neither claimed nor refused"), "{all}");
        assert!(all.contains(said), "{all}");
        assert!(all.contains(&output_path(&dir, "sp-x").display().to_string()), "{all}");
        assert!(!all.contains("journal"), "{all}");
    }

    #[test_id(tetwyc8)]
    #[test]
    fn the_last_thing_a_silent_dwarf_said_is_its_last_fatal_else_its_last_line_else_nothing() {
        assert_eq!(last_said("a\nFATAL one\nlater\n\n"), "FATAL one");
        assert_eq!(last_said("a\nFATAL one\nFATAL two\nlater\n"), "FATAL two");
        assert_eq!(last_said("first\nlast\n\n"), "last");
        assert_eq!(last_said("\n \n"), "the dwarf printed nothing");
        assert_eq!(last_said(""), "the dwarf printed nothing");
    }

    #[test_id(tzldjek)]
    #[test]
    fn a_stale_output_log_is_not_quoted_for_a_dwarf_that_printed_nothing() {
        let dir = testkit::TempDir::new("dwarf-direct-stale");
        std::fs::create_dir_all(output_path(&dir, "sp-x").parent().unwrap()).unwrap();
        std::fs::write(output_path(&dir, "sp-x"), "FATAL an earlier dwarf\n").unwrap();
        let f = { let mut f = fake(row("READY", None), 0); f.run_dir = dir.to_path_buf(); f };
        let (rc, sink) = go(&f);
        assert_eq!(rc, 1);
        let all = sink.all();
        assert!(all.contains("the dwarf printed nothing") && !all.contains("an earlier dwarf"), "{all}");
    }

    #[test_id(ti8zrev)]
    #[test]
    fn a_refusal_is_left_where_the_launcher_looks_for_that_bead() {
        let dir = testkit::TempDir::new("dwarf-direct-refusal-path");
        write_refusal(&dir, "sp-x", "because");
        assert_eq!(std::fs::read_to_string(refusal_path(&dir, "sp-x")).unwrap(), "because\n");
    }
}

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

use std::path::Path;

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

#[allow(clippy::too_many_arguments)]
pub fn run(exec: &dyn Exec, sink: &dyn Sink, req: &Request, home: &Path, dwarf_path: Option<&str>, call_bin: &str, call_argv: &[String], now: i64) -> i32 {
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
    o.code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{MemSink, Out};
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
    }
    impl Exec for Fake {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            self.calls.lock().unwrap().push((prog.to_string(), args.to_vec()));
            match prog {
                "spira-lc" => self.show.clone(),
                "slay" => Out { code: self.slay_rc, stdout: String::new(), stderr: "slay said no".into() },
                _ => Out::ok(""),
            }
        }
    }
    fn row(state: &str, holder: Option<&str>) -> Out {
        let h = holder.map_or("null".to_string(), |h| format!("\"{h}\""));
        Out::ok(format!(r#"{{"bead":{{"bead_id":"sp-x","state":"{state}","holder":{h},"lease_until":null,"holds":"","express":0,"stack":"{{}}"}}}}"#))
    }
    fn go(f: &Fake) -> (i32, MemSink) {
        let sink = MemSink::default();
        let rc = run(f, &sink, &req(), Path::new("/h"), Some("/rel/bin/dwarf"), "systemd-run", &[], 1000);
        (rc, sink)
    }
    fn progs(f: &Fake) -> Vec<String> {
        f.calls.lock().unwrap().iter().map(|c| c.0.clone()).collect()
    }

    #[test]
    fn a_ready_bead_is_called_without_a_takeover() {
        let f = Fake { show: row("READY", None), slay_rc: 0, calls: Mutex::default() };
        let (rc, _) = go(&f);
        assert_eq!((rc, progs(&f)), (0, a(&["spira-lc", "systemd-run"])));
    }

    #[test]
    fn a_held_bead_is_taken_over_with_its_work_kept_and_the_requester_in_the_why() {
        let f = Fake { show: row("WORKING", Some("dwarf-old")), slay_rc: 0, calls: Mutex::default() };
        let (rc, sink) = go(&f);
        assert_eq!(rc, 0);
        assert_eq!(progs(&f), a(&["spira-lc", "slay", "spira-lc", "systemd-run"]), "the old dwarf stops and its leftover hold is lifted before the new one starts");
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[2].1, a(&["unhold", "sp-x", "wait", "dwarf"]));
        assert_eq!(calls[1].1, a(&["--bead", "sp-x", "--keep-work", "--why", "directed by concierge: loop fix"]));
        assert!(calls[3].1.windows(2).any(|w| w == ["--replaced", "dwarf-old"]), "{:?}", calls[3].1);
        assert!(sink.all().contains("stopped dwarf-old"));
    }

    #[test]
    fn a_dwarf_that_will_not_stop_means_nothing_is_directed() {
        let f = Fake { show: row("WORKING", Some("dwarf-old")), slay_rc: 1, calls: Mutex::default() };
        let (rc, sink) = go(&f);
        assert_eq!((rc, progs(&f)), (1, a(&["spira-lc", "slay"])));
        assert!(sink.all().contains("slay said no"), "{}", sink.all());
    }

    #[test]
    fn a_bead_past_the_builder_or_an_unreadable_row_is_refused_before_anything_runs() {
        let f = Fake { show: row("SUBMITTED", None), slay_rc: 0, calls: Mutex::default() };
        assert_eq!((go(&f).0, progs(&f)), (1, a(&["spira-lc"])));
        let f = Fake { show: Out { code: 2, stdout: String::new(), stderr: "store down".into() }, slay_rc: 0, calls: Mutex::default() };
        assert_eq!((go(&f).0, progs(&f)), (1, a(&["spira-lc"])));
        let f = Fake { show: Out { code: 1, stdout: String::new(), stderr: String::new() }, slay_rc: 0, calls: Mutex::default() };
        assert_eq!((go(&f).0, progs(&f)), (0, a(&["spira-lc", "systemd-run"])), "no row yet: the claim creates it");
    }
}

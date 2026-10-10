//! `aeon stop <bead> [--why <text>]`: ask the live aeon on a bead to stop. Nothing signals a
//! pid and nothing reads `/proc`; the aeon's own heartbeat sees the request and trips its
//! session, so the stop is as visible as any other end of a session.

use std::path::{Path, PathBuf};

pub fn request_file(run: &Path, bead: &str) -> PathBuf {
    run.join("aeon").join(format!("{bead}.stop"))
}

pub fn requested(run: &Path, bead: &str) -> bool {
    request_file(run, bead).is_file()
}

pub fn clear(run: &Path, bead: &str) {
    let _ = std::fs::remove_file(request_file(run, bead));
}

/// The identity pidfile of the aeon whose lease is running on `bead`.
pub fn live_identity(run: &Path, bead: &str, now: i64) -> Option<PathBuf> {
    let suffix = format!("-{bead}.pid");
    std::fs::read_dir(run).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("aeon-") && n.ends_with(&suffix))
            && strand::probe::lease_live(p, now)
    })
}

/// Record the stop request. `Err` names the state that refuses it: no aeon holds `bead`.
pub fn request(run: &Path, bead: &str, why: &str, now: i64) -> Result<String, String> {
    let Some(pf) = live_identity(run, bead, now) else {
        return Err(format!("no live aeon on {bead}: no identity lease is running for it"));
    };
    let name = std::fs::read_to_string(pf.with_extension("name")).unwrap_or_default();
    let name = if name.trim().is_empty() { "?" } else { name.trim() };
    let why = if why.is_empty() { "stopped by the operator" } else { why };
    std::fs::write(run.join(format!("{bead}.slain")), format!("{now}\t{why}\n")).map_err(|e| format!("cannot record the stop: {e}"))?;
    let f = request_file(run, bead);
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(&f, format!("{why}\n")).map_err(|e| format!("cannot record the stop: {e}"))?;
    Ok(format!("aeon {name} on {bead} will stop at its next heartbeat"))
}

/// The CLI: `<bead> [--why <text>]`. Exit 0 requested, 1 refused (names the state), 2 usage.
pub fn run(run: &Path, args: &[String], now: i64) -> i32 {
    let mut bead = None;
    let mut why = String::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--why" => match it.next() {
                Some(w) => why = w.clone(),
                None => return usage(),
            },
            b if !b.starts_with('-') && bead.is_none() => bead = Some(b.to_string()),
            _ => return usage(),
        }
    }
    let Some(bead) = bead else { return usage() };
    match request(run, &bead, &why, now) {
        Ok(m) => {
            println!("{m}");
            0
        }
        Err(e) => {
            eprintln!("aeon stop: {e}");
            1
        }
    }
}

fn usage() -> i32 {
    eprintln!("usage: aeon stop <bead> [--why <text>]");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aeon_on(run: &Path, bead: &str, deadline: i64) {
        let pf = run.join(format!("aeon-builder-{bead}.pid"));
        std::fs::write(&pf, "1\n").unwrap();
        std::fs::write(pf.with_extension("name"), "ifrit").unwrap();
        strand::probe::write_lease(&pf, deadline);
    }

    #[test]
    fn a_running_lease_accepts_a_stop_and_records_it() {
        let t = testkit::TempDir::new("aeon-stop-ok");
        aeon_on(&t, "sp-a", 1000);
        let m = request(&t, "sp-a", "because", 500).unwrap();
        assert!(m.contains("ifrit") && m.contains("sp-a"), "{m}");
        assert!(requested(&t, "sp-a"));
        assert!(std::fs::read_to_string(t.join("sp-a.slain")).unwrap().contains("because"));
        clear(&t, "sp-a");
        assert!(!requested(&t, "sp-a"));
    }

    #[test]
    fn an_expired_or_absent_lease_refuses_naming_the_state() {
        let t = testkit::TempDir::new("aeon-stop-refuse");
        aeon_on(&t, "sp-a", 400);
        let e = request(&t, "sp-a", "", 500).unwrap_err();
        assert!(e.contains("no live aeon on sp-a"), "{e}");
        assert!(request(&t, "sp-nobody", "", 500).is_err());
        assert!(!requested(&t, "sp-a") && !t.join("sp-a.slain").exists(), "a refusal records nothing");
    }

    #[test]
    fn the_stop_is_for_that_bead_only() {
        let t = testkit::TempDir::new("aeon-stop-scope");
        aeon_on(&t, "sp-a", 1000);
        aeon_on(&t, "sp-ab", 1000);
        request(&t, "sp-a", "", 500).unwrap();
        assert!(!requested(&t, "sp-ab"));
    }

    #[test]
    fn usage_is_exit_2() {
        let t = testkit::TempDir::new("aeon-stop-usage");
        assert_eq!(run(&t, &[], 0), 2);
        assert_eq!(run(&t, &["--why".into()], 0), 2);
        assert_eq!(run(&t, &["sp-a".into(), "sp-b".into()], 0), 2);
        assert_eq!(run(&t, &["sp-none".into()], 0), 1);
    }
}

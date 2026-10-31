//! For one fixture world, bead-sweep.sh and the bead-sweep handler leave the same rows in the
//! Concierge inbox. The script is the oracle: it runs unmodified against a stubbed `spira-lc`;
//! the handler runs on a virtual clock against a fake spira-lc socket serving the same files.
//! Every threshold is pinned to a non-default on both sides.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use serde_json::{json, Value};
use spira_runtime::watchers::bead_sweep::BeadSweep;
use spira_runtime::watchers::iso8601;
use spira_runtime::world::World;
use spira_runtime::Effects;

const ASK: &str = "needs-boss";
const HOME: &str = "widgets";
const MIN: u64 = 60;
const HOUR: u64 = 3600;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn sweep() -> BeadSweep {
    BeadSweep { interval_ms: 1000, ask_label: ASK.into(), home_repo: HOME.into(), hold_min: 30, rework_min: 5, cert_min: 5, wait_min: 45, batch_min: 60, stop_review_secs: 3600 }
}

fn row(id: &str, state: &str, age: u64, holds: &[&str], blocked: &[&str], reason: &str) -> Value {
    json!({"bead_id": id, "state": state, "since": now_secs() - age, "holds": json!(holds).to_string(), "blocked_by": blocked, "reason": reason})
}

fn bead(id: &str, labels: &[&str]) -> Value {
    json!({"id": id, "labels": labels, "issue_type": "task", "status": "open", "priority": 1, "title": "t"})
}

struct Fixture {
    dir: testkit::TempDir,
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn put(&self, name: &str, v: &Value) {
        fs::write(self.p(name), v.to_string()).unwrap();
    }

    fn build(name: &str, rows: &[Value], content: &[Value]) -> Fixture {
        let f = Fixture { dir: testkit::TempDir::new(&format!("sweep-{name}")) };
        for d in ["bin", "run/watchd", "run/scoped-stops", "queue/widgets"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        f.put("rows.json", &json!(rows));
        f.put("content.json", &json!(content));
        f.put("term.json", &json!([{"bead_id": "sp-done", "state": "LANDED"}]));
        fs::write(f.p("creates.log"), "").unwrap();
        let root = f.dir.path().display();
        testkit::write_exe(
            f.p("bin/spira-lc"),
            &format!(
                "#!/usr/bin/env bash\n[ -e {root}/down ] && exit 1\ncase \"$1\" in\n  list) case \"$3\" in LANDED*) cat {root}/term.json ;; *) cat {root}/rows.json ;; esac ;;\n  content) cat {root}/content.json ;;\n  history) cat {root}/history-$2.json ;;\n  create-bead) [ -e {root}/refuse-create ] && exit 2; echo \"$*\" >> {root}/creates.log ;;\nesac\nexit 0\n"
            ),
        );
        fs::write(f.p("rows"), format!("{HOME} | {}/w | queue | origin/main | | |\nprrepo | {}/p | pr | origin/main | | |\n", f.dir.path().display(), f.dir.path().display())).unwrap();
        fs::write(
            f.p("layer.toml"),
            format!(
                "[spira]\nrun = \"{}\"\nask_label = \"{ASK}\"\nhome_repo = \"{HOME}\"\nrepo_map = \"{}\"\nqueue_dir = \"{}\"\nconcierge_inbox = \"{}\"\nscoped_stop_review_secs = 3600\n",
                f.p("run").display(),
                f.p("rows").display(),
                f.p("queue").display(),
                f.p("old-inbox").display()
            ),
        )
        .unwrap();
        f
    }

    fn serve_lc(&self) -> PathBuf {
        let sock = self.p("lc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let root = self.dir.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let argv: Vec<String> = serde_json::from_str(&line).unwrap();
                let (code, body) = if root.join("down").exists() {
                    (1, "boom".to_string())
                } else {
                    match argv[0].as_str() {
                        "list" if argv[2].starts_with("LANDED") => (0, fs::read_to_string(root.join("term.json")).unwrap()),
                        "list" => (0, fs::read_to_string(root.join("rows.json")).unwrap()),
                        "content" => (0, fs::read_to_string(root.join("content.json")).unwrap()),
                        "history" => (0, fs::read_to_string(root.join(format!("history-{}.json", argv[1]))).unwrap_or_default()),
                        "create-bead" if root.join("refuse-create").exists() => (2, "refused".into()),
                        "create-bead" => {
                            let mut log = fs::OpenOptions::new().append(true).open(root.join("creates.log")).unwrap();
                            writeln!(log, "{}", argv.join(" ")).unwrap();
                            (0, String::new())
                        }
                        _ => (0, String::new()),
                    }
                };
                let mut w = &stream;
                writeln!(w, "{{\"exit_code\": {code}, \"len\": {}}}", body.len()).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
        });
        sock
    }
}

fn debug_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn rows_of(inbox: &Path) -> Vec<String> {
    fs::read_to_string(inbox).unwrap_or_default().lines().map(|l| l.split_once(' ').unwrap().1.to_string()).collect()
}

fn old_rows(f: &Fixture, ticks: usize) -> Vec<String> {
    assert!(debug_dir().join("spira-config").exists(), "build spira-config first: bead-sweep.sh resolves its settings through it");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/bead-sweep.sh");
    let path = format!("{}:{}:/usr/bin:/bin", f.p("bin").display(), debug_dir().display());
    let status = Command::new("bash")
        .arg(&script)
        .args(["watch", "--interval", "1", "--ticks", &ticks.to_string()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", f.dir.path())
        .env("BEAD_SWEEP_HOLD_MIN", "30")
        .env("BEAD_SWEEP_REWORK_MIN", "5")
        .env("BEAD_SWEEP_CERT_MIN", "5")
        .env("BEAD_SWEEP_WAIT_MIN", "45")
        .env("BEAD_SWEEP_BATCH_MIN", "60")
        .env("SPIRA_TOML", format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira-config/tests/fixtures/complete.toml").display(), f.p("layer.toml").display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success() || f.p("down").exists(), "bead-sweep.sh failed");
    rows_of(&f.p("old-inbox")).into_iter().map(without_claimability).collect()
}

/// The script asks `spira-claim stack` about a stale REWORK row; spira-lc has no door to that answer, so the handler says the row and not its claimability.
fn without_claimability(row: String) -> String {
    match row.split_once("; claimability unknown: ") {
        Some((head, tail)) => format!("{head}){}", tail.split_once(')').map_or("", |t| t.1)),
        None => row,
    }
}

fn new_rows(f: &Fixture, ticks: usize) -> Vec<String> {
    let world = World {
        run_dir: f.p("run"),
        queue_dir: f.p("queue"),
        repo_map: f.p("rows"),
        inbox: f.p("new-inbox"),
        lc_socket: f.serve_lc(),
        ask_label: ASK.into(),
        home_repo: HOME.into(),
        ..World::default()
    };
    let fx = Effects { world, ..Effects::default() };
    let clock = VirtualClock::new(now_secs() * 1000);
    let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![sweep().registration()], 0).unwrap();
    let (id, stream, kind) = BeadSweep::boot();
    e.submit(id, stream, kind).unwrap();
    for _ in 0..ticks {
        e.run_until_idle().unwrap();
        loop {
            let pending = e.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => Outcome::Failed(d),
                };
                e.complete(&p.key, out).unwrap();
            }
            e.run_until_idle().unwrap();
        }
        clock.advance(1000);
    }
    rows_of(&f.p("new-inbox"))
}

fn same_rows(name: &str, ticks: usize, rows: &[Value], content: &[Value], prep: impl Fn(&Fixture)) -> (Vec<String>, Fixture) {
    let f = Fixture::build(name, rows, content);
    prep(&f);
    let old = old_rows(&f, ticks);
    let g = Fixture::build(name, rows, content);
    prep(&g);
    let new = new_rows(&g, ticks);
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    assert_eq!(sorted(new), sorted(old.clone()), "scenario {name}: the handler and bead-sweep.sh disagree (the host runs a pass's appends concurrently, so line order is not compared)");
    assert_eq!(fs::read_to_string(g.p("creates.log")).unwrap(), fs::read_to_string(f.p("creates.log")).unwrap(), "scenario {name}: the repairs differ");
    (old, g)
}

fn date_in(secs: i64) -> String {
    iso8601((now_secs() as i64 + secs) as u64)[..10].to_string()
}

fn has(r: &[String], needle: &str) -> bool {
    r.iter().any(|l| l.contains(needle))
}

#[test]
fn e2e_every_suspect_class_is_announced_once_across_repeated_passes() {
    let old = 3 * HOUR;
    let rows = [
        row("sp-pois", "READY", 0, &["poison"], &[], "bad tip"),
        row("sp-ask", "READY", 0, &["ask"], &[], "which repo"),
        row("sp-man", "READY", 50 * MIN, &["manual"], &[], "r"),
        row("sp-edge", "READY", 0, &[], &["sp-done"], "r"),
        row("sp-rw", "REWORK", 8 * MIN, &[], &[], "r"),
        row("sp-cert", "CERTIFIED", 8 * MIN, &[], &[], "r"),
        row("sp-wait", "SUBMITTED", 90 * MIN, &[], &[], "r"),
        row("sp-blk", "READY", 0, &[], &["sp-pois"], "r"),
        row("sp-askl", "READY", 0, &[], &[], "r"),
        row("sp-lease", "WORKING", old, &[], &[], "r"),
    ];
    let mut rows = rows.to_vec();
    rows[9]["lease_until"] = json!(now_secs() - 100);
    rows[9]["holder"] = json!("dwarf-with-a-very-long-name-that-is-cut@1");
    let mut numeric = row("sp-lease2", "WORKING", old, &[], &[], "r");
    numeric["holder"] = json!(12345);
    numeric["lease_until"] = json!((now_secs() - 100).to_string());
    rows.push(numeric);
    let mut undated = row("sp-undated", "REWORK", 0, &[], &[], "r");
    undated["since"] = json!(null);
    undated["updated_at"] = json!(now_secs() - 8 * MIN);
    rows.push(undated);
    let content = [bead("sp-cert", &["repo:other"]), bead("sp-rowless", &[]), bead("sp-askl", &[ASK])];
    let (r, _) = same_rows("classes", 3, &rows, &content, |_| {});
    for needle in ["SUSPECT sp-pois READY POISONED", "SUSPECT sp-ask READY ASK HOLD", "SUSPECT sp-man READY STALE HOLD manual, over 30m", "SUSPECT sp-edge READY STALE EDGE", "SUSPECT sp-rw REWORK UNCLAIMED REWORK over 5m", "SUSPECT sp-cert CERTIFIED CERTIFIED outside repo:widgets", "SUSPECT sp-wait SUBMITTED WAITING over 45m", "SUSPECT sp-lease WORKING LEASE EXPIRED holder dwarf-with-a-very-long-name-", "SUSPECT sp-lease2 WORKING LEASE EXPIRED holder 12345", "SUSPECT sp-undated REWORK UNCLAIMED REWORK over 5m"] {
        assert_eq!(r.iter().filter(|l| l.contains(needle)).count(), 1, "{needle}: {r:?}");
    }
    assert!(has(&r, "REPAIRED sp-rowless"), "{r:?}");
    assert!(!has(&r, "sp-blk") && !has(&r, "sp-askl"), "only SUSPECT is announced: {r:?}");
}

#[test]
fn e2e_a_manual_hold_is_judged_by_its_review_date_read_from_the_row_or_the_hold_history() {
    let (fut, past) = (date_in(10 * 86_400), date_in(-3 * 86_400));
    let rows = [
        row("sp-hfut", "READY", 3 * HOUR, &["manual"], &[], &format!("review-by:{fut} waits on tenancy")),
        row("sp-hpast", "READY", 3 * HOUR, &["manual"], &[], &format!("review-by:{past} waits on tenancy")),
        row("sp-hnone", "READY", 3 * HOUR, &["manual"], &[], "waits on tenancy"),
        row("sp-hnew", "READY", 10 * MIN, &["manual"], &[], "waits on tenancy"),
        row("sp-hlost", "READY", 3 * HOUR, &["manual"], &[], "reply: m1"),
        row("sp-hlate", "READY", 3 * HOUR, &["manual"], &[], "reply: m2"),
    ];
    let lost = json!([{"event": "Hold", "applied": 1, "evidence": {"Hold": {"kind": "Manual", "detail": format!("review-by:{past} waits on data")}}}]);
    let unheld = json!([{"event": "Hold", "applied": 1, "evidence": format!("{{\"detail\": \"review-by:{past} old\"}}")}, {"event": "Unhold", "applied": 1}, {"event": "Hold", "applied": 1, "evidence": {"detail": "no date"}}]);
    let (r, _) = same_rows("holds", 2, &rows, &[], |f| {
        f.put("history-sp-hlost.json", &lost);
        f.put("history-sp-hlate.json", &unheld);
    });
    assert!(has(&r, &format!("SUSPECT sp-hpast READY HOLD REVIEW OVERDUE since {past} (waits on tenancy)")), "{r:?}");
    assert!(has(&r, &format!("SUSPECT sp-hlost READY HOLD REVIEW OVERDUE since {past} (waits on data)")), "{r:?}");
    assert!(has(&r, "SUSPECT sp-hnone READY STALE HOLD manual, over 30m, no review-by (waits on tenancy)"), "{r:?}");
    assert!(has(&r, "SUSPECT sp-hlate READY STALE HOLD"), "a hold after an unhold is judged on its own: {r:?}");
    assert!(!has(&r, "sp-hfut") && !has(&r, "sp-hnew"), "{r:?}");
}

#[test]
fn e2e_an_open_batch_a_stop_a_reservation_and_an_integration_each_explain_a_wait() {
    let rows = [
        row("sp-q", "CERTIFIED", 8 * MIN, &[], &[], "r"),
        row("sp-bm", "IN_DELIVERY", 90 * MIN, &[], &[], "r"),
        row("sp-srw", "REWORK", 3 * HOUR, &[], &[], "r"),
        row("sp-rsv", "REWORK", 3 * HOUR, &[], &[], "r"),
        row("sp-scert", "SUBMITTED", 3 * HOUR, &[], &[], "r"),
        row("sp-m1", "SUBMITTED", 3 * HOUR, &[], &[], "r"),
        row("sp-m2", "CERTIFIED", 3 * HOUR, &[], &[], "r"),
        row("sp-int", "READY", 3 * HOUR, &[], &[], "r"),
        row("sp-free", "SUBMITTED", 3 * HOUR, &[], &[], "r"),
        row("sp-prw", "SUBMITTED", 3 * HOUR, &[], &[], "r"),
        row("sp-solo", "SUBMITTED", 3 * HOUR, &[], &[], "r"),
        row("sp-young", "CERTIFIED", 3 * MIN, &[], &[], "r"),
    ];
    let content = [
        bead("sp-q", &["repo:other"]),
        bead("sp-bm", &["repo:other"]),
        bead("sp-srw", &["repo:stopped"]),
        bead("sp-rsv", &["repo:widgets"]),
        bead("sp-scert", &["repo:stopped"]),
        bead("sp-m1", &["repo:widgets"]),
        bead("sp-m2", &["repo:widgets"]),
        bead("sp-int", &["repo:widgets"]),
        bead("sp-free", &["repo:widgets"]),
        bead("sp-prw", &["repo:prrepo"]),
        bead("sp-solo", &["repo:widgets"]),
        bead("sp-young", &["repo:other"]),
    ];
    let since = now_secs() - 2 * HOUR;
    let (r, _) = same_rows("waits", 2, &rows, &content, |f| {
        fs::write(f.p("run/scoped-stops/repo-stopped.json"), json!({"repo": "stopped", "lanes": ["claims", "rounds"], "owner": "concierge", "until": "fix lands", "since": since}).to_string()).unwrap();
        fs::write(f.p("run/scoped-stops/reserve-sp-rsv.json"), json!({"bead": "sp-rsv", "owner": "concierge", "reason": "hand fix", "until": "", "since": since}).to_string()).unwrap();
        fs::write(f.p("run/scoped-stops/reserve-torn.json"), "{").unwrap();
        fs::write(f.p("run/scoped-stops/repo-stopped2.json"), json!({"repo": "stopped", "lanes": ["claims", "rounds"], "owner": "later-owner", "until": "fix lands", "since": since}).to_string()).unwrap();
        fs::write(f.p("queue/widgets/integrations"), "sp-int sp-m1 sp-m2\n").unwrap();
        fs::create_dir_all(f.p("run/solo-skips")).unwrap();
        fs::write(f.p("run/solo-skips/sp-solo"), "2\n").unwrap();
    });
    assert!(has(&r, "SUSPECT sp-q CERTIFIED CERTIFIED outside"), "{r:?}");
    assert!(has(&r, "SUSPECT sp-int READY INTEGRATION STALLED over 45m in READY; members sp-m1 sp-m2"), "{r:?}");
    assert!(has(&r, "SUSPECT sp-free SUBMITTED WAITING over 45m for a round"), "{r:?}");
    assert!(has(&r, "SUSPECT sp-solo SUBMITTED WAITING over 45m for a round (solo: starved)"), "{r:?}");
    assert!(has(&r, "SUSPECT - SCOPED stop stopped claims,rounds OVERDUE: owner later-owner, release condition: fix lands"), "{r:?}");
    assert!(has(&r, "SUSPECT - SCOPED reservation sp-rsv OVERDUE: owner concierge, release condition: the bead lands"), "{r:?}");
    for quiet in ["sp-srw", "sp-rsv", "sp-scert", "sp-m1", "sp-m2", "sp-prw", "sp-young"] {
        assert!(!has(&r, &format!("SUSPECT {quiet} ")), "{quiet} is explained, not suspect: {r:?}");
    }
}

#[test]
fn e2e_an_open_bead_with_no_row_is_given_one_and_a_refused_repair_stays_a_suspect() {
    let orphan = json!({"id": "sp-orph", "labels": ["repo:widgets"], "issue_type": "task", "status": "open", "priority": 0, "title": "fix\tthe thing"});
    let norepo = json!({"id": "sp-norepo", "labels": ["spira"], "issue_type": "task", "status": "open", "priority": 3, "title": "Warden sweep landed"});
    let idea = json!({"id": "sp-idea", "labels": ["spira"], "issue_type": "proposal", "status": "open", "priority": 2, "title": "a parked idea"});
    let busy = json!({"id": "sp-busy", "labels": ["repo:widgets"], "issue_type": "task", "status": "in_progress", "priority": 2, "title": "claimed"});
    let (r, g) = same_rows("repair", 2, &[], &[orphan.clone(), norepo, idea, busy], |_| {});
    assert!(has(&r, "REPAIRED sp-orph open bead had no lifecycle row; created it (priority 0, repo widgets)"), "{r:?}");
    assert!(has(&r, "REPAIRED sp-norepo open bead had no lifecycle row; created it (priority 3, repo none)"), "{r:?}");
    let log = fs::read_to_string(g.p("creates.log")).unwrap();
    assert!(log.contains("create-bead sp-orph --priority 0 --title fix the thing --repo widgets"), "{log}");
    assert!(log.contains("create-bead sp-norepo --priority 3 --title Warden sweep landed\n"), "{log}");
    assert!(!log.contains("sp-idea") && !has(&r, "sp-idea"), "a parked idea gets no row: {r:?}");
    assert!(has(&r, "SUSPECT sp-busy ROWLESS"), "an in-progress rowless bead is reported, not created: {r:?}");

    let (r, _) = same_rows("refused", 2, &[], &[orphan], |f| fs::write(f.p("refuse-create"), "").unwrap());
    assert_eq!(r.iter().filter(|l| l.contains("SUSPECT sp-orph ROWLESS")).count(), 1, "{r:?}");
    assert!(!has(&r, "REPAIRED"), "{r:?}");
}

#[test]
fn e2e_a_healthy_board_is_quiet_and_a_blind_pass_announces_nothing() {
    let rows = [row("sp-ok", "READY", 0, &[], &[], "r")];
    let content = [bead("sp-ok", &["repo:widgets"])];
    assert!(same_rows("healthy", 2, &rows, &content, |_| {}).0.is_empty());
    let down = [row("sp-pois", "READY", 0, &["poison"], &[], "r")];
    assert!(same_rows("blind", 2, &down, &[], |f| fs::write(f.p("down"), "").unwrap()).0.is_empty());
}

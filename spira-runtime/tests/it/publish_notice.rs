//! A publish PR's opened, red, merged or closed line reaches the Concierge inbox when the scheduler
//! observes it, not at `pr-notify`'s own poll — through the real engine and host, with that poll
//! set to half an hour.

use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::ports::{Clock, VirtualClock};
use runtime::{EffectSpec, MemJournal, NullMarker, NullSink};
use serde_json::{json, Value};
use spira_runtime::tasks::a::{publish, queue_step};
use spira_runtime::watchers::{pr_notify, INBOX_APPEND};
use spira_runtime::{open_engine, tasks, BOOT_STAGGER_MS};

const NOW: u64 = 1_794_294_300_000;
const TWO_MIN: u64 = 120_000;
const HEAD: &str = "abcdef0123456789";
const TITLE: &str = "publish: abcdef012345 for spira";

#[derive(Clone, Debug)]
struct Ask {
    name: String,
    input: String,
    at: u64,
}

type Script = Arc<dyn Fn(&str, u64, &[Ask]) -> Option<String> + Send + Sync>;

struct Runner {
    clock: VirtualClock,
    asked: Mutex<Vec<Ask>>,
    script: Script,
}

impl EffectRunner for Runner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let at = self.clock.now_ms();
        let mut asked = self.asked.lock().unwrap();
        asked.push(Ask { name: spec.name.clone(), input: spec.input.clone(), at });
        if let Some(answer) = (self.script)(&spec.name, at, &asked) {
            return Ok(answer);
        }
        Ok(match spec.name.as_str() {
            queue_step::OBSERVE | publish::OBSERVE => json!({ "repos": {}, "world_running": true }).to_string(),
            pr_notify::OBSERVE => json!({ "repos": [] }).to_string(),
            publish::PUSH_BRANCH => json!({ "repo": "spira", "branch": "spira/publish/1" }).to_string(),
            publish::PR_CREATE => json!({ "pr": 1301 }).to_string(),
            _ => "{}".to_string(),
        })
    }
}

fn queue_snapshot(pr: u64, check: &str, fail: &str) -> String {
    let repo = json!({
        "mode": "queue.local", "lc_ok": true, "open_batch": null,
        "publish_rec": { "pr": pr, "head": HEAD, "branch": "spira/publish/1" },
        "publish_check": check, "publish_fail": fail, "pr_state": "none", "check": "none",
        "base_sha": "b1", "sealed_base": null, "lock_skips": 0, "provisional_reds": []
    });
    json!({ "repos": { "spira": repo }, "world_running": true }).to_string()
}

fn publish_snapshot(rec: Option<(u64, &str)>, pr: Value, pr_word: &str, head_sha: &str, forge_sha: &str, at: u64) -> String {
    let rec = rec.map(|(pr, head)| json!({ "pr": pr, "head": head, "branch": "spira/publish/1", "rerun": false, "alarmed": false }));
    let repo = json!({
        "mode": "queue.local", "publish_mode": "pr", "unpublished": false, "stopped": false,
        "publish_rec": rec, "pr": pr, "pr_word": pr_word, "open_publish_prs": [],
        "forge_sha": forge_sha, "head_sha": head_sha, "diverged": false, "red_marker": null,
        "fix_forward_live": false, "round_holds_hypervisor": false, "observed_at": at / 1000
    });
    json!({ "repos": { "spira": repo }, "world_running": true }).to_string()
}

fn open_pr(ci: &str, behind: bool) -> Value {
    json!({ "open": { "ci": ci, "behind": behind, "tries": 0 } })
}

fn ran(asked: &[Ask], name: &str) -> bool {
    asked.iter().any(|a| a.name == name)
}

const BOOTED: [&str; 3] = ["publish/boot", "queue.step/boot", "pr-notify/boot"];

fn run(script: Script, until: u64) -> Vec<Ask> {
    let clock = VirtualClock::new(NOW);
    let runner = Arc::new(Runner { clock: clock.clone(), asked: Mutex::new(vec![]), script });
    let mut settings = spira_runtime::settings::fixture();
    settings.pr_notify.interval_ms = 1_800_000;
    let mut engine = open_engine(&settings, MemJournal::new(), Arc::new(clock.clone()), Arc::new(NullMarker), Arc::new(NullSink), vec![], std::time::Duration::from_secs(10)).unwrap();
    let now = engine.now_ms();
    let mut found = 0;
    for (id, stream, kind) in tasks::boots() {
        if BOOTED.contains(&id) {
            engine.start_at(stream, kind, now + found * BOOT_STAGGER_MS);
            found += 1;
        }
    }
    assert_eq!(found, BOOTED.len() as u64, "every task these tests read has a boot tick");
    let mut d = VirtualDriver::new(engine, clock, runner.clone());
    d.run_until(NOW + until).unwrap();
    let asked = runner.asked.lock().unwrap().clone();
    asked
}

fn lines(asked: &[Ask]) -> Vec<(String, u64)> {
    asked.iter().filter(|a| a.name == INBOX_APPEND && a.input.contains("[watch:pr-notify]")).map(|a| (a.input.split("[watch:pr-notify] ").nth(1).unwrap_or_default().to_string(), a.at)).collect()
}

fn first_at(asked: &[Ask], name: &str) -> u64 {
    asked.iter().find(|a| a.name == name).unwrap_or_else(|| panic!("{name} never ran")).at
}

fn said_by(asked: &[Ask], prefix: &str, observed: &str) -> Vec<u64> {
    let t = first_at(asked, observed);
    lines(asked).into_iter().filter(|(l, _)| l.starts_with(prefix)).map(|(_, at)| at - t.min(at)).collect()
}

#[test_id(tcee24c)]
#[test]
fn a_red_publish_check_is_in_the_inbox_within_two_minutes_and_once_while_it_stays() {
    let script: Script = Arc::new(|name, _, _| (name == queue_step::OBSERVE).then(|| queue_snapshot(1301, "red", "build,lint")));
    let asked = run(script, 3 * queue_step::CADENCE_MS + 30_000);
    assert!(asked.iter().filter(|a| a.name == queue_step::OBSERVE).count() >= 3, "red must stand across three ticks");
    let said = lines(&asked);
    assert_eq!(said.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), [format!("FAIL RED #1301 {TITLE} [spira]: build,lint")]);
    assert!(said_by(&asked, "FAIL RED", queue_step::OBSERVE)[0] <= TWO_MIN);
}

fn publish_notice(script: impl Fn(&str, u64, &[Ask]) -> Option<String> + Send + Sync + 'static, prefix: &str) -> Vec<u64> {
    let asked = run(Arc::new(script), 10 * 60_000);
    let at = said_by(&asked, prefix, publish::OBSERVE);
    let all = lines(&asked);
    assert_eq!(all.iter().filter(|(l, _)| l.starts_with(prefix)).count(), 1, "{all:?}");
    at
}

#[test_id(t2ul38j)]
#[test]
fn a_created_publish_pr_is_opened_in_the_inbox_within_two_minutes() {
    let at = publish_notice(
        |name, now, asked| {
            if name != publish::OBSERVE {
                return None;
            }
            Some(if ran(asked, publish::PR_CREATE) { publish_snapshot(Some((1301, HEAD)), open_pr("pending", false), "OPEN", HEAD, "f0", now) } else { publish_snapshot(None, json!("unknown"), "", HEAD, "f0", now) })
        },
        &format!("OPENED #1301 {TITLE}"),
    );
    assert!(at[0] <= TWO_MIN);
}

#[test_id(taqn3pz)]
#[test]
fn a_superseded_publish_pr_is_closed_in_the_inbox_under_its_own_title() {
    let at = publish_notice(
        |name, now, asked| {
            if name != publish::OBSERVE {
                return None;
            }
            Some(if ran(asked, publish::PR_CLOSE) { publish_snapshot(None, json!("unknown"), "", "h2", "h2", now) } else { publish_snapshot(Some((1302, HEAD)), open_pr("red", true), "OPEN", "h2", "f0", now) })
        },
        &format!("CLOSED #1302 {TITLE}"),
    );
    assert!(at[0] <= TWO_MIN);
}

#[test_id(tv3kqkq)]
#[test]
fn a_settled_publish_pr_is_merged_in_the_inbox_within_two_minutes() {
    let at = publish_notice(
        |name, now, asked| match name {
            publish::OBSERVE if ran(asked, publish::SETTLE) => Some(publish_snapshot(None, json!("unknown"), "", HEAD, HEAD, now)),
            publish::OBSERVE => Some(publish_snapshot(Some((1304, HEAD)), open_pr("green", false), "OPEN", HEAD, "f0", now)),
            publish::SETTLE => Some(json!({ "settled": true, "why": "" }).to_string()),
            _ => None,
        },
        &format!("MERGED #1304 {TITLE}"),
    );
    assert!(at[0] <= TWO_MIN);
}

#[test_id(tclepck)]
#[test]
fn a_settle_that_waited_says_nothing() {
    let script: Script = Arc::new(|name, now, _| match name {
        publish::OBSERVE => Some(publish_snapshot(Some((1304, HEAD)), open_pr("green", false), "OPEN", HEAD, "f0", now)),
        publish::SETTLE => Some(json!({ "settled": false, "why": "PR 1304 CI is pending" }).to_string()),
        _ => None,
    });
    let asked = run(script, 10 * 60_000);
    assert!(ran(&asked, publish::SETTLE));
    assert_eq!(lines(&asked), []);
}

#[test_id(tag9nms)]
#[test]
fn a_publish_pr_the_forge_shows_gone_is_said_in_the_state_it_went() {
    for (word, line) in [("MERGED", "MERGED"), ("CLOSED", "CLOSED")] {
        let at = publish_notice(
            move |name, now, asked| (name == publish::OBSERVE).then(|| if ran(asked, publish::RETIRE) { publish_snapshot(None, json!("unknown"), "", HEAD, HEAD, now) } else { publish_snapshot(Some((1305, HEAD)), json!("gone"), word, HEAD, HEAD, now) }),
            &format!("{line} #1305 {TITLE}"),
        );
        assert!(at[0] <= TWO_MIN, "{word}");
    }
}

fn poll_script(settles: bool) -> Script {
    Arc::new(move |name, now, asked| match name {
        queue_step::OBSERVE if settles && now >= NOW + 10 * 60_000 && !ran(asked, publish::SETTLE) => Some(queue_snapshot(1304, "green", "")),
        publish::OBSERVE if settles && now >= NOW + 10 * 60_000 && !ran(asked, publish::SETTLE) => Some(publish_snapshot(Some((1304, HEAD)), open_pr("green", false), "OPEN", HEAD, "f0", now)),
        publish::SETTLE => Some(json!({ "settled": true, "why": "" }).to_string()),
        pr_notify::OBSERVE => {
            let n = asked.iter().filter(|a| a.name == pr_notify::OBSERVE).count();
            let open = |v: Vec<Value>| Some(json!({ "repos": [["spira", { "open": v, "finals": {}, "runs": {}, "strays": null }]] }).to_string());
            if n == 1 {
                open(vec![json!({ "number": 1304, "status": "pending", "title": "held title", "fail": "" })])
            } else {
                Some(json!({ "repos": [["spira", { "open": [], "finals": { "1304": { "state": "MERGED", "sha": "0123456789abcdef", "beads": [] } }, "runs": {}, "strays": null }]] }).to_string())
            }
        }
        _ => None,
    })
}

#[test_id(taj0t6b)]
#[test]
fn the_later_poll_repeats_no_line_still_mails_the_merge_and_asks_what_it_asked() {
    let until = 70 * 60_000;
    let with = run(poll_script(true), until);
    let without = run(poll_script(false), until);

    let owned: Vec<&str> = tasks::all().filter(|t| BOOTED.contains(&(t.boot)().0)).flat_map(|t| t.effects.iter().copied()).collect();
    for asked in [&with, &without] {
        let strangers: Vec<&str> = asked.iter().map(|a| a.name.as_str()).filter(|n| ![INBOX_APPEND, pr_notify::OPERATOR_NOTE].contains(n) && !owned.contains(n)).collect();
        assert_eq!(strangers, Vec::<&str>::new(), "only the three tasks run");
        assert!(asked.len() < 200, "{} asks", asked.len());
    }

    let merged = lines(&with).into_iter().filter(|(l, _)| l.starts_with("MERGED #1304")).collect::<Vec<_>>();
    assert_eq!(merged.len(), 1, "{:?}", lines(&with));
    assert!(merged[0].0.contains("held title"), "{}", merged[0].0);
    assert!(merged[0].1 < NOW + 30 * 60_000, "said at the event, not at the poll");
    let late = lines(&without).into_iter().filter(|(l, _)| l.starts_with("MERGED #1304")).collect::<Vec<_>>();
    assert_eq!(late.len(), 1, "the poll alone says it once, late");
    assert!(late[0].1 >= NOW + 30 * 60_000);

    let notes = |a: &[Ask]| a.iter().filter(|x| x.name == pr_notify::OPERATOR_NOTE && x.input.contains("MERGED #1304")).count();
    assert_eq!((notes(&with), notes(&without)), (1, 1));
    let observes = |a: &[Ask]| a.iter().filter(|x| x.name == pr_notify::OBSERVE).map(|x| serde_json::from_str::<pr_notify::Carry>(&x.input).unwrap().repos.get("spira").map(|r| r.prs.keys().copied().collect::<Vec<_>>())).collect::<Vec<_>>();
    assert_eq!(observes(&with), observes(&without));
}

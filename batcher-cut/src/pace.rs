//! The per-repo pacing a round scheduler owes its operator: hold on a red, back off on a
//! harness fault, page an orphaned open round. State lives in files under
//! `<run>/rounds-pace/<repo>/` so one repo's hold never touches another's and an operator
//! lifts a hold with `rm`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const INFRA_MARKERS: &[&str] = &["NO RESULT", "OVER-CAP", "cannot check out", "certify rc=75", "left no verdicts"];
pub const STALE_AMI_MARKER: &str = "round AMI is stale";
pub const BACKOFF_SECS: u64 = 600;
pub const STOP_AFTER: u32 = 2;
pub const REFUSAL_WINDOW_SECS: u64 = 1800;
pub const REFUSAL_STOP_AFTER: u32 = 2;

#[derive(Default, Clone, PartialEq, Debug)]
pub struct State {
    pub infra: u32,
    pub backoff_until: u64,
    pub paged_open: String,
}

#[derive(PartialEq, Debug)]
pub enum Outcome {
    Quiet,
    Red,
    Infra,
    StaleImage,
    OpenFailed,
}

#[derive(PartialEq, Debug)]
pub enum Hold {
    Paused,
    Stopped,
    Backoff(u64),
}

#[derive(Default, PartialEq, Debug)]
pub struct Effects {
    pub pause: bool,
    pub stop: bool,
    pub pages: Vec<String>,
}

/// The cut's result and the repo's local verdict (`verdict`, `at`) as written during the pass.
/// A cut that admitted nothing is `Quiet` whatever its wording: only a round that ran and did
/// not land is red.
pub fn classify(result: &Result<(), String>, verdict: Option<(&str, u64)>, pass_start: u64) -> Outcome {
    let text = result.as_ref().err().map(String::as_str).unwrap_or("");
    if text.contains(STALE_AMI_MARKER) {
        return Outcome::StaleImage;
    }
    if INFRA_MARKERS.iter().any(|m| text.contains(m)) {
        return Outcome::Infra;
    }
    let red_verdict = matches!(verdict, Some(("red", at)) if at >= pass_start);
    if text.contains("NOT LANDED") || (result.is_ok() && red_verdict) {
        return Outcome::Red;
    }
    if text.contains("OPEN FAILED") {
        return Outcome::OpenFailed;
    }
    Outcome::Quiet
}

pub fn hold(paused: bool, stopped: bool, st: &State, now: u64) -> Option<Hold> {
    if stopped {
        Some(Hold::Stopped)
    } else if paused {
        Some(Hold::Paused)
    } else if st.backoff_until > now {
        Some(Hold::Backoff(st.backoff_until))
    } else {
        None
    }
}

pub fn advance(st: &State, outcome: &Outcome, repo: &str, now: u64, detail: &str) -> (State, Effects) {
    let mut next = st.clone();
    let mut fx = Effects::default();
    match outcome {
        Outcome::Quiet => next.infra = 0,
        Outcome::Red => {
            next.infra = 0;
            fx.pause = true;
            fx.pages.push(format!("ROUND RED {repo} — attribute and eject, then rm the pause file ({detail})"));
        }
        Outcome::StaleImage => {}
        Outcome::OpenFailed => {
            fx.pages.push(format!("OPEN FAILED {repo} — the cut could not open a round ({detail})"));
        }
        Outcome::Infra => {
            next.infra += 1;
            next.backoff_until = now + BACKOFF_SECS;
            if next.infra >= STOP_AFTER {
                fx.stop = true;
                fx.pages.push(format!(
                    "ROUNDS STOPPED {repo}: {} consecutive rounds produced no verdicts — VM/pool fault, look before lifting the stop ({detail})",
                    next.infra
                ));
            }
        }
    }
    (next, fx)
}

/// An open round at the start of a pass has no driver (the pass is the only driver and passes
/// do not overlap): page it once per batch, never re-cut it.
pub fn open_page(st: &State, repo: &str, open_id: &str) -> (State, Option<String>) {
    if st.paged_open == open_id {
        return (st.clone(), None);
    }
    let mut next = st.clone();
    next.paged_open = open_id.to_string();
    (next, Some(format!("ROUND OPEN {repo} {open_id} with no driver — finish, eject or abandon it")))
}

pub struct Files {
    dir: PathBuf,
}

impl Files {
    pub fn new(run: &Path, repo: &str) -> Files {
        Files { dir: run.join("rounds-pace").join(repo) }
    }
    pub fn pause_file(&self) -> PathBuf {
        self.dir.join("pause")
    }
    pub fn stop_file(&self) -> PathBuf {
        self.dir.join("stopped")
    }
    pub fn load(&self) -> State {
        let kv: BTreeMap<String, String> = fs::read_to_string(self.dir.join("state"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        State {
            infra: kv.get("infra").and_then(|v| v.parse().ok()).unwrap_or(0),
            backoff_until: kv.get("backoff_until").and_then(|v| v.parse().ok()).unwrap_or(0),
            paged_open: kv.get("paged_open").cloned().unwrap_or_default(),
        }
    }
    pub fn save(&self, st: &State) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let body = format!("infra={}\nbackoff_until={}\npaged_open={}\n", st.infra, st.backoff_until, st.paged_open);
        fs::write(self.dir.join("state"), body).map_err(|e| format!("rounds-pace state: {e}"))
    }
    fn touch(&self, p: PathBuf) -> Result<(), String> {
        fs::create_dir_all(&self.dir).and_then(|_| fs::write(&p, "")).map_err(|e| format!("{}: {e}", p.display()))
    }
}

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Refusals {
    pub key: String,
    pub at: u64,
    pub count: u32,
}

/// A refusal's text with the per-round tokens (batch id, object ids) removed, so the same
/// refusal on a fresh round compares equal.
pub fn refusal_key(msg: &str, batch: &str) -> String {
    msg.split_whitespace()
        .filter(|t| !(t.len() >= 40 && t.chars().all(|c| c.is_ascii_hexdigit())) && !t.contains(batch))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn note_refusal(prev: &Refusals, key: &str, now: u64) -> Refusals {
    let repeat = prev.key == key && now.saturating_sub(prev.at) <= REFUSAL_WINDOW_SECS;
    Refusals { key: key.to_string(), at: now, count: if repeat { prev.count + 1 } else { 1 } }
}

impl Files {
    fn refusals_path(&self) -> PathBuf {
        self.dir.join("refusals")
    }
    pub fn load_refusals(&self) -> Refusals {
        let text = fs::read_to_string(self.refusals_path()).unwrap_or_default();
        let mut lines = text.lines();
        let (at, count) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
        Refusals { at: at.parse().unwrap_or(0), count: count.parse().unwrap_or(0), key: lines.collect::<Vec<_>>().join(" ") }
    }
    pub fn clear_refusals(&self) {
        let _ = fs::remove_file(self.refusals_path());
    }
    /// Records a refusal; at the second like it inside the window the repo is stopped and the
    /// text to page is returned.
    pub fn record_refusal(&self, repo: &str, key: &str, now: u64) -> Result<Option<String>, String> {
        let next = note_refusal(&self.load_refusals(), key, now);
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(self.refusals_path(), format!("{}\n{}\n{}\n", next.at, next.count, next.key)).map_err(|e| format!("rounds-pace refusals: {e}"))?;
        if next.count < REFUSAL_STOP_AFTER {
            return Ok(None);
        }
        self.touch(self.stop_file())?;
        Ok(Some(format!("ROUNDS STOPPED {repo}: {} consecutive rounds refused for the same reason — rm the stopped file once fixed ({key})", next.count)))
    }
    pub fn stopped(&self) -> bool {
        self.stop_file().exists()
    }
}

pub struct Seams<'a> {
    pub repo: &'a str,
    pub now: u64,
    pub inbox: &'a Path,
    pub stamp: &'a dyn Fn(u64) -> String,
}

fn page(sm: &Seams, text: &str) -> Result<(), String> {
    let mut f = fs::OpenOptions::new().create(true).append(true).open(sm.inbox).map_err(|e| format!("{}: {e}", sm.inbox.display()))?;
    writeln!(f, "{} [watch:rounds] {text}", (sm.stamp)(sm.now)).map_err(|e| format!("{}: {e}", sm.inbox.display()))
}

/// One repo's paced cut. Returns the cut's own result so a failure still fails the pass.
pub fn paced(
    files: &Files,
    sm: &Seams,
    open_round: impl FnOnce() -> Result<Option<String>, String>,
    verdict: impl FnOnce() -> Option<(String, u64)>,
    cut: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut st = files.load();
    if let Some(h) = hold(files.pause_file().exists(), files.stop_file().exists(), &st, sm.now) {
        println!("batcher rounds: {}: held ({h:?}) — no cut", sm.repo);
        return Ok(());
    }
    if let Some(id) = open_round()? {
        let (next, pg) = open_page(&st, sm.repo, &id);
        if let Some(text) = pg {
            page(sm, &text)?;
        }
        files.save(&next)?;
        println!("batcher rounds: {}: round {id} is open — not re-cut", sm.repo);
        return Ok(());
    }
    let res = cut();
    let v = verdict();
    let outcome = classify(&res, v.as_ref().map(|(s, at)| (s.as_str(), *at)), sm.now);
    let detail = res.as_ref().err().cloned().unwrap_or_default();
    let (next, fx) = advance(&st, &outcome, sm.repo, sm.now, &detail);
    st = next;
    if fx.pause {
        files.touch(files.pause_file())?;
    }
    if fx.stop {
        files.touch(files.stop_file())?;
    }
    for p in &fx.pages {
        page(sm, p)?;
    }
    files.save(&st)?;
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn dir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("pace-{tag}"))
    }
    fn stamp(n: u64) -> String {
        format!("T{n}")
    }
    fn run(d: &Path, repo: &str, now: u64, open: Option<&str>, verdict: Option<(&str, u64)>, res: Result<(), String>) -> bool {
        let inbox = d.join("inbox.log");
        let sm = Seams { repo, now, inbox: &inbox, stamp: &stamp };
        let cut_ran = Cell::new(false);
        let r = paced(&Files::new(d, repo), &sm, || Ok(open.map(String::from)), || verdict.map(|(s, a)| (s.to_string(), a)), || {
            cut_ran.set(true);
            res
        });
        let _ = r;
        cut_ran.get()
    }
    fn inbox(d: &Path) -> String {
        fs::read_to_string(d.join("inbox.log")).unwrap_or_default()
    }

    #[test]
    fn a_refusal_repeated_inside_the_window_stops_the_repo_once_and_a_new_reason_does_not() {
        let t = dir("refusal");
        let f = Files::new(t.path(), "a");
        let key = refusal_key("refused at 0123456789abcdef0123456789abcdef01234567 — round r-9 holds [x] but built from [y]", "r-9");
        assert!(!key.contains("0123456789abcdef"));
        assert_eq!(f.record_refusal("a", &key, 1000).unwrap(), None);
        assert!(!f.stopped(), "one abandon only records");
        assert_eq!(f.record_refusal("a", "other", 1010).unwrap(), None);
        let page = f.record_refusal("a", "other", 1040).unwrap().expect("the second like refusal alarms");
        assert!(page.contains("ROUNDS STOPPED a") && f.stopped());
        let g = Files::new(t.path(), "b");
        g.record_refusal("b", "k", 1000).unwrap();
        assert_eq!(g.record_refusal("b", "k", 1000 + REFUSAL_WINDOW_SECS + 1).unwrap(), None, "outside the window it is a first");
    }

    #[test]
    fn a_not_landed_round_pauses_and_pages_and_the_pause_holds_the_next_cut() {
        let t = dir("red");
        let d = t.path();
        let ran = run(d, "a", 1000, None, None, Err("NOT LANDED: suite x red".into()));
        assert!(ran);
        assert!(Files::new(d, "a").pause_file().exists());
        assert!(inbox(d).contains("T1000 [watch:rounds] ROUND RED a"));
        let ran = run(d, "a", 2000, None, None, Ok(()));
        assert!(!ran, "paused repo must not cut");
        fs::remove_file(Files::new(d, "a").pause_file()).unwrap();
        let ran = run(d, "a", 3000, None, None, Ok(()));
        assert!(ran, "rm of the pause file lifts it");
    }

    #[test]
    fn a_red_local_verdict_written_in_the_pass_is_a_red_round_but_a_stale_one_is_not() {
        let t = dir("verdict");
        let d = t.path();
        run(d, "a", 1000, None, Some(("red", 999)), Ok(()));
        assert!(!Files::new(d, "a").pause_file().exists(), "a verdict from before the pass is not this pass's");
        run(d, "a", 1000, None, Some(("red", 1005)), Ok(()));
        assert!(Files::new(d, "a").pause_file().exists());
    }

    #[test]
    fn a_cut_that_admitted_nothing_never_pauses_whatever_its_wording() {
        let t = dir("empty");
        let d = t.path();
        for (i, msg) in ["nothing admissible", "pool empty", "queue is empty"].iter().enumerate() {
            run(d, "a", 1000 + i as u64 * 100, None, None, Err(msg.to_string()));
            run(d, "a", 1050 + i as u64 * 100, None, None, Ok(()));
        }
        assert!(!Files::new(d, "a").pause_file().exists());
        assert!(!Files::new(d, "a").stop_file().exists());
        assert_eq!(inbox(d), "");
    }

    #[test]
    fn a_round_with_no_verdicts_backs_off_and_the_second_in_a_row_stops_and_pages() {
        let t = dir("infra");
        let d = t.path();
        let infra = || Err("round left no verdicts".to_string());
        run(d, "a", 1000, None, None, infra());
        assert_eq!(inbox(d), "");
        let ran = run(d, "a", 1000 + BACKOFF_SECS - 1, None, None, infra());
        assert!(!ran, "inside the backoff there is no retry");
        let ran = run(d, "a", 1000 + BACKOFF_SECS, None, None, infra());
        assert!(ran);
        assert!(Files::new(d, "a").stop_file().exists());
        assert!(inbox(d).contains("ROUNDS STOPPED a: 2 consecutive"));
        let ran = run(d, "a", 9_000_000, None, None, Ok(()));
        assert!(!ran, "stopped stays stopped until the operator removes the file");
    }

    #[test]
    fn a_clean_round_resets_the_infra_streak() {
        let t = dir("reset");
        let d = t.path();
        run(d, "a", 1000, None, None, Err("OVER-CAP".into()));
        run(d, "a", 2000, None, None, Ok(()));
        assert_eq!(Files::new(d, "a").load().infra, 0);
        run(d, "a", 3000, None, None, Err("OVER-CAP".into()));
        assert!(!Files::new(d, "a").stop_file().exists());
    }

    #[test]
    fn a_repeated_open_failed_pages_every_time_and_does_not_pause() {
        let t = dir("openfail");
        let d = t.path();
        run(d, "a", 1000, None, None, Err("OPEN FAILED: x".into()));
        run(d, "a", 1100, None, None, Err("OPEN FAILED: x".into()));
        assert_eq!(inbox(d).matches("OPEN FAILED a").count(), 2);
        assert!(!Files::new(d, "a").pause_file().exists());
    }

    #[test]
    fn an_open_round_is_never_re_cut_and_is_paged_once_per_batch() {
        let t = dir("open");
        let d = t.path();
        let ran = run(d, "a", 1000, Some("b-1"), None, Ok(()));
        assert!(!ran);
        run(d, "a", 1100, Some("b-1"), None, Ok(()));
        assert_eq!(inbox(d).matches("ROUND OPEN a b-1 with no driver").count(), 1);
        run(d, "a", 1200, Some("b-2"), None, Ok(()));
        assert_eq!(inbox(d).matches("ROUND OPEN").count(), 2);
    }

    #[test]
    fn one_repos_red_does_not_hold_another() {
        let t = dir("repos");
        let d = t.path();
        run(d, "a", 1000, None, None, Err("NOT LANDED".into()));
        let ran = run(d, "b", 1010, None, None, Ok(()));
        assert!(ran);
        assert!(!Files::new(d, "b").pause_file().exists());
    }

    #[test]
    fn a_stale_round_image_never_counts_toward_the_stop() {
        let t = dir("stale-ami");
        let d = t.path();
        let stale = "certify rc=75: round AMI is stale: acquire failed: clone refused";
        assert_eq!(classify(&Err(stale.into()), None, 0), Outcome::StaleImage);
        for at in [1000, 2000, 3000] {
            run(d, "a", at, None, None, Err(stale.into()));
        }
        assert!(!Files::new(d, "a").stop_file().exists());
        assert_eq!(Files::new(d, "a").load().infra, 0);
    }

    #[test]
    fn the_matcher_could_have_found_a_marker() {
        assert_eq!(classify(&Err("x certify rc=75".into()), None, 0), Outcome::Infra);
        assert_eq!(classify(&Err("fine".into()), None, 0), Outcome::Quiet);
    }
}

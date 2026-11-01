//! The replay fixtures are typed records, not text: every line of every file in
//! `sim/fixtures/replay/<slice>/` deserialises into its closed shape, with unknown fields
//! refused and every id, repo, sha and stamp in its anonymised form. A field of free text added
//! by a later capture fails here, before a reader starts to depend on it.

use serde::Deserialize;
use std::path::{Path, PathBuf};

const SLICES: [&str; 2] = ["1031", "1021-1025"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join("replay")
}

macro_rules! checked_string {
    ($name:ident, $check:expr, $what:literal) => {
        #[derive(Debug, Deserialize)]
        #[serde(try_from = "String")]
        #[allow(dead_code)]
        struct $name(String);
        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(s: String) -> Result<Self, String> {
                let ok: fn(&str) -> bool = $check;
                if ok(&s) {
                    Ok($name(s))
                } else {
                    Err(format!("{:?} is not {}", s, $what))
                }
            }
        }
    };
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

fn repo(s: &str) -> bool {
    s.len() == 6 && s.starts_with("repo-") && (b'a'..=b'l').contains(&s.as_bytes()[5])
}

fn bead(s: &str) -> bool {
    s.strip_prefix("b-").is_some_and(|rest| rest.split('.').all(digits))
}

fn hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `<repo>-<YYYYMMDDTHHMMSSZ>`, `<repo>-r<n>-<MMDDTHHMMSS>`, `<repo>-q-<YYYYMMDDTHHMMSSZ>` or
/// `<repo>-solo-<bead>`: the batch's own timestamp kept, its repo anonymised.
fn batch(s: &str) -> bool {
    let (Some(r), Some(rest)) = (s.get(..6), s.get(7..)) else { return false };
    let ts = |t: &str, date: usize| {
        let b = t.as_bytes();
        b.len() == date + 7 && b[date] == b'T' && digits(&t[..date]) && digits(&t[date + 1..date + 7])
    };
    let full = |t: &str| t.len() == 16 && t.ends_with('Z') && ts(&t[..15], 8);
    repo(r)
        && s.as_bytes()[6] == b'-'
        && (full(rest)
            || rest.strip_prefix("q-").is_some_and(full)
            || rest.strip_prefix("solo-").is_some_and(bead)
            || rest.strip_prefix('r').and_then(|t| t.split_once('-')).is_some_and(|(n, t)| digits(n) && ts(t, 4)))
}

fn code(s: &str) -> bool {
    s.len() <= 41 && s.as_bytes().first().is_some_and(u8::is_ascii_lowercase) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

checked_string!(Stamp, stamp, "a UTC stamp YYYY-MM-DDTHH:MM:SSZ");
checked_string!(Repo, repo, "an anonymised repo (repo-a..repo-l)");
checked_string!(Bead, bead, "an anonymised bead id (b-<n>)");
checked_string!(Batch, batch, "an anonymised batch id");
checked_string!(Sha, |s| hex(s, 12), "a 12-hex anonymised sha");
checked_string!(PassKey, |s| hex(s, 16), "a 16-hex pass key");
checked_string!(Code, code, "a closed reason code (a short kebab word)");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum BatchState {
    None,
    Open,
    CiRunning,
    Green,
    Attributing,
    Rebuilding,
    Abandoned,
    Landed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum BeadState {
    Open,
    Ready,
    Working,
    Done,
    Submitted,
    Certified,
    InDelivery,
    Rework,
    Landed,
    Superseded,
    Dropped,
}

/// The lifecycle's own batch event names (`lifecycle::batch`).
#[derive(Debug, Deserialize)]
enum BatchEvent {
    MemberAdded,
    CiStarted,
    PassStarted,
    SuitesStarted,
    Failed,
    PassGreen,
    PassRed,
    PassIncomplete,
    PassRebuilt,
    Green,
    BaseMoved,
    Eject,
    FastForward,
    Abandon,
}

/// The lifecycle's own bead event names (`lifecycle::bead`), heartbeats excepted.
#[derive(Debug, Deserialize)]
enum BeadEvent {
    Claim,
    Release,
    HolderDead,
    Done,
    Submit,
    Resubmit,
    Sifted,
    GatePass,
    GateRed,
    GateInfra,
    Deliver,
    Delivered,
    ContentOnBase,
    Requeued,
    Returned,
    Hold,
    Unhold,
    Reply,
    AskWithdrawn,
    Express,
    Absolve,
    Disposition,
    Supersede,
    Drop,
    Reclassify,
}

/// A round line's cause: the closed `tsd::pass::Cause` set for the end of a pass, otherwise the
/// lifecycle batch event that moved the round.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum RoundCause {
    Pass(PassCause),
    Event(BatchEvent),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PassCause {
    Green,
    RedAttributed,
    OverCap,
    VmLost,
    Infra,
    ContentionFlake,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Blocked,
    Refused,
    Ejected,
    Green,
    LandedLocal,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Place {
    Ec2,
    Proxmox,
    Local,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Member {
    bead: Bead,
    tip: Sha,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Round {
    at: Stamp,
    repo: Repo,
    batch: Batch,
    state_from: BatchState,
    state_to: BatchState,
    members: Vec<Member>,
    parent: Option<Batch>,
    cause: RoundCause,
    verdict: Option<Verdict>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct CertPass {
    at: Stamp,
    repo: Repo,
    batch: Batch,
    pass: Option<u32>,
    pass_key: Option<PassKey>,
    cause: PassCause,
    place: Place,
    wall_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
#[allow(dead_code)]
enum Transition {
    Batch {
        at: Stamp,
        id: Batch,
        from: BatchState,
        to: BatchState,
        event: BatchEvent,
        member: Option<Bead>,
        tip: Option<Sha>,
        sha: Option<Sha>,
        n: Option<u32>,
        suites_s: Option<u64>,
        build_s: Option<u64>,
        red_suites: Option<u32>,
    },
    Bead {
        at: Stamp,
        id: Bead,
        from: BeadState,
        to: BeadState,
        event: BeadEvent,
        tip: Option<Sha>,
        code: Option<Code>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StopKind {
    Halted,
    Released,
    Paused,
    Unpaused,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Lane {
    Claims,
    Rounds,
    Publish,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Stop {
    at: Stamp,
    kind: StopKind,
    repo: Option<Repo>,
    lanes: Option<Vec<Lane>>,
    bead: Option<Bead>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Land {
    at: Stamp,
    repo: Repo,
    bead: Bead,
    merge_sha: Option<Sha>,
    reverts: Option<Bead>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Delivered,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct AlarmAttempt {
    at: Stamp,
    caller: Code,
    kind: Code,
    outcome: Outcome,
    reason_code: Code,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Attribution {
    at: Stamp,
    repo: Repo,
    batch: Batch,
    pass: u32,
    owner: Vec<Bead>,
    innocent: Vec<Bead>,
    wall_secs: u64,
}

fn parse<T: for<'de> Deserialize<'de>>(text: &str) -> Result<usize, String> {
    let mut n = 0;
    for (i, line) in text.lines().enumerate() {
        serde_json::from_str::<T>(line).map_err(|e| format!("line {}: {e}: {line}", i + 1))?;
        n += 1;
    }
    Ok(n)
}

/// Every file of one slice, each line into its type; the count of lines read per file.
fn load_slice(dir: &Path) -> Result<Vec<(&'static str, usize)>, String> {
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).map_err(|e| format!("{}: {e}", dir.join(f).display()));
    let mut out = Vec::new();
    let mut each = |f: &'static str, p: fn(&str) -> Result<usize, String>| -> Result<(), String> {
        out.push((f, p(&read(f)?).map_err(|e| format!("{f}: {e}"))?));
        Ok(())
    };
    each("round.jsonl", parse::<Round>)?;
    each("cert_pass.jsonl", parse::<CertPass>)?;
    each("transition.jsonl", parse::<Transition>)?;
    each("stop.jsonl", parse::<Stop>)?;
    each("land.jsonl", parse::<Land>)?;
    each("alarm_attempt.jsonl", parse::<AlarmAttempt>)?;
    each("attribution.jsonl", parse::<Attribution>)?;
    Ok(out)
}

#[test]
fn every_line_of_every_slice_deserialises_into_its_closed_shape() {
    for slice in SLICES {
        let t0 = std::time::Instant::now(); // clock-safe: reported against the 100 ms goal, never asserted
        let counts = load_slice(&root().join(slice)).unwrap_or_else(|e| panic!("{slice}: {e}"));
        let ms = t0.elapsed().as_millis(); // clock-safe: reported, never asserted
        let lines: usize = counts.iter().map(|(_, n)| n).sum();
        eprintln!("fixture {slice}: {lines} lines in {ms} ms (goal: under 100 ms per slice) {counts:?}");
        for (f, n) in &counts {
            if *f != "stop.jsonl" {
                assert!(*n > 0, "{slice}/{f} is empty");
            }
        }
    }
}

#[test]
fn a_line_with_an_extra_text_field_is_refused() {
    let text = std::fs::read_to_string(root().join("1031").join("round.jsonl")).unwrap();
    let first = text.lines().next().unwrap();
    assert!(parse::<Round>(first).is_ok(), "the unplanted line parses: {first}");
    let planted = format!("{},\"note\":\"stacked on the parent, which never landed\"}}", first.trim_end_matches('}'));
    let err = parse::<Round>(&planted).expect_err("a free-text field must be refused");
    assert!(err.contains("unknown field `note`"), "{err}");
}

#[test]
fn a_real_id_in_place_of_an_anonymised_one_is_refused() {
    let line = r#"{"at":"2026-10-31T00:00:00Z","repo":"repo-a","bead":"sp-abc123"}"#;
    let err = parse::<Land>(line).expect_err("a real bead id must be refused");
    assert!(err.contains("not an anonymised bead id"), "{err}");
}

/// throughput-bounds §1's git land column for Pacific 10-12..10-31 (10-31 to the moment it was counted).
const GIT_LAND: [i64; 20] = [77, 104, 158, 119, 180, 103, 108, 33, 100, 93, 65, 101, 77, 107, 34, 52, 72, 65, 65, 19];
const CAUSES: [&str; 5] = ["hand-landing", "day-boundary", "relanded", "batch-multi-member", "unexplained"];

#[test]
fn landed_20d_carries_the_git_column_and_a_cause_for_every_nonzero_delta() {
    let text = std::fs::read_to_string(root().join("landed-20d.tsv")).unwrap();
    let mut rows = text.lines().filter(|l| !l.starts_with('#'));
    assert_eq!(rows.next(), Some("day\tlifecycle_landed\tgit_land\thand_landed_dir\tdelta\tcause"));
    let rows: Vec<Vec<&str>> = rows.map(|l| l.split('\t').collect()).collect();
    assert_eq!(rows.len(), 20, "one row per Pacific day 10-12..10-31");
    let git: Vec<i64> = rows.iter().map(|r| r[2].parse().unwrap()).collect();
    assert_eq!(git, GIT_LAND);
    for r in &rows {
        assert_eq!(r.len(), 6, "{r:?}");
        let n = |i: usize| r[i].parse::<i64>().unwrap_or_else(|e| panic!("{r:?} col {i}: {e}"));
        assert_eq!(n(4), n(1) - n(2), "delta is lifecycle minus git: {r:?}");
        assert!(n(3) >= 0);
        if n(4) != 0 {
            assert_ne!(r[5], "-", "a nonzero delta has a cause: {r:?}");
        }
        if r[5] != "-" {
            assert!(r[5].split('+').all(|c| CAUSES.contains(&c)), "cause outside the closed set: {r:?}");
        }
    }
}

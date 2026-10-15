//! One record per certification pass, whatever repo or lane ran it: the `cert-pass` family.
//! A pass that ends with no record is a lost verified result, so the record is a closed shape
//! that refuses what it cannot place: an unknown cause is an error, never a free-text string.

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub const FAMILY: &str = "cert-pass";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Green,
    RedAttributed,
    OverCap,
    VmLost,
    Infra,
    ContentionFlake,
}

impl Cause {
    pub const ALL: [Cause; 6] = [Cause::Green, Cause::RedAttributed, Cause::OverCap, Cause::VmLost, Cause::Infra, Cause::ContentionFlake];

    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Green => "green",
            Cause::RedAttributed => "red-attributed",
            Cause::OverCap => "over-cap",
            Cause::VmLost => "vm-lost",
            Cause::Infra => "infra",
            Cause::ContentionFlake => "contention-flake",
        }
    }

    pub fn parse(s: &str) -> Result<Cause, String> {
        Cause::ALL.into_iter().find(|c| c.as_str() == s).ok_or_else(|| {
            let all: Vec<&str> = Cause::ALL.iter().map(|c| c.as_str()).collect();
            format!("loss cause {s:?} is not one of {}", all.join(", "))
        })
    }

    /// The verdict a pass with this cause can carry.
    pub fn verdict(self) -> &'static str {
        match self {
            Cause::Green => "green",
            Cause::RedAttributed | Cause::ContentionFlake => "red",
            Cause::OverCap | Cause::VmLost | Cause::Infra => "incomplete",
        }
    }

    /// From a lane's exit code: 0 green, 1 red, 124/137 the wall, else the lane or the VM was lost.
    pub fn from_exit(rc: i32, vm_lost: bool) -> Cause {
        match rc {
            0 => Cause::Green,
            1 => Cause::RedAttributed,
            124 | 137 => Cause::OverCap,
            _ if vm_lost => Cause::VmLost,
            _ => Cause::Infra,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Ec2,
    Proxmox,
    Local,
}

impl Place {
    pub fn as_str(self) -> &'static str {
        match self {
            Place::Ec2 => "ec2",
            Place::Proxmox => "proxmox",
            Place::Local => "local",
        }
    }

    pub fn parse(s: &str) -> Result<Place, String> {
        [Place::Ec2, Place::Proxmox, Place::Local].into_iter().find(|p| p.as_str() == s).ok_or_else(|| format!("where {s:?} is not one of ec2, proxmox, local"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassRecord {
    pub repo: String,
    pub batch: String,
    pub pass: String,
    pub members: Vec<String>,
    pub instance_type: String,
    pub vcpus: Option<u32>,
    pub place: Place,
    pub capacity_wait_s: u64,
    pub boot_s: u64,
    pub build_s: u64,
    pub suites_s: u64,
    pub cause: Cause,
    pub detail: String,
}

impl PassRecord {
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [("repo", &self.repo), ("batch", &self.batch), ("pass", &self.pass)] {
            if v.trim().is_empty() {
                return Err(format!("a pass record needs a {name}"));
            }
        }
        Ok(())
    }

    pub fn fields(&self) -> Vec<(String, Value)> {
        let f = |k: &str, v: Value| (k.to_string(), v);
        vec![
            f("repo", self.repo.clone().into()),
            f("batch", self.batch.clone().into()),
            f("pass", self.pass.clone().into()),
            f("members", self.members.clone().into()),
            f("instance_type", self.instance_type.clone().into()),
            f("vcpus", self.vcpus.map_or(Value::Null, Value::from)),
            f("where", self.place.as_str().into()),
            f("capacity_wait_s", self.capacity_wait_s.into()),
            f("boot_s", self.boot_s.into()),
            f("build_s", self.build_s.into()),
            f("suites_s", self.suites_s.into()),
            f("verdict", self.cause.verdict().into()),
            f("cause", self.cause.as_str().into()),
            f("detail", self.detail.clone().into()),
        ]
    }

    pub fn from_row(row: &Value) -> Result<PassRecord, String> {
        let s = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("row has no {k}"));
        let n = |k: &str| row.get(k).and_then(Value::as_u64).ok_or_else(|| format!("row has no numeric {k}"));
        let rec = PassRecord {
            repo: s("repo")?,
            batch: s("batch")?,
            pass: s("pass")?,
            members: row.get("members").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            instance_type: s("instance_type")?,
            vcpus: row.get("vcpus").and_then(Value::as_u64).map(|v| v as u32),
            place: Place::parse(&s("where")?)?,
            capacity_wait_s: n("capacity_wait_s")?,
            boot_s: n("boot_s")?,
            build_s: n("build_s")?,
            suites_s: n("suites_s")?,
            cause: Cause::parse(&s("cause")?)?,
            detail: s("detail").unwrap_or_default(),
        };
        rec.validate()?;
        Ok(rec)
    }

    /// Appends this pass's one row to `<root>/tsd/cert-pass.jsonl`.
    pub fn write(&self, root: &Path, host: &str, ts: &str) -> Result<(), String> {
        self.validate()?;
        let line = crate::build_row(ts, host, FAMILY, &self.fields())?;
        crate::append_line(&crate::family_path(root, FAMILY), &line)
    }
}

/// Checks that every `(repo, batch, pass)` expected has exactly one row in `text`, and counts
/// passes by key, not lines: a line that is not a pass record is not a pass.
pub fn exactly_one(text: &str, expected: &[(&str, &str, &str)]) -> Result<(), String> {
    let mut seen: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for line in text.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        if row.get("family").and_then(Value::as_str) != Some(FAMILY) {
            continue;
        }
        if let Ok(r) = PassRecord::from_row(&row) {
            *seen.entry((r.repo, r.batch, r.pass)).or_default() += 1;
        }
    }
    let mut bad = Vec::new();
    for (repo, batch, pass) in expected {
        let n = seen.get(&(repo.to_string(), batch.to_string(), pass.to_string())).copied().unwrap_or(0);
        if n != 1 {
            bad.push(format!("{repo}/{batch}/{pass}: {n} records"));
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad.join("; "))
    }
}

/// Reads `key=value` fields from a CLI into a record: the door for lanes that are not round-vm.
pub fn from_args(args: &[String]) -> Result<PassRecord, String> {
    let mut m: Map<String, Value> = Map::new();
    let mut members = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].strip_prefix("--").ok_or_else(|| format!("unexpected argument {:?}", args[i]))?;
        i += 1;
        let val = args.get(i).ok_or_else(|| format!("--{key} needs a value"))?;
        i += 1;
        match key {
            "members" => members = val.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect(),
            "repo" | "batch" | "pass" | "instance-type" | "where" | "cause" | "detail" => {
                m.insert(key.replace('-', "_"), Value::String(val.clone()));
            }
            "vcpus" | "capacity-wait-s" | "boot-s" | "build-s" | "suites-s" => {
                let n: u64 = val.parse().map_err(|_| format!("--{key} must be a whole number"))?;
                m.insert(key.replace('-', "_"), Value::from(n));
            }
            other => return Err(format!("unknown option --{other}")),
        }
    }
    m.insert("members".into(), members.into());
    m.entry("instance_type").or_insert(Value::String(String::new()));
    for k in ["capacity_wait_s", "boot_s", "build_s", "suites_s"] {
        m.entry(k).or_insert(Value::from(0));
    }
    m.insert("where".into(), m.get("where").cloned().ok_or("--where is required")?);
    PassRecord::from_row(&Value::Object(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(batch: &str, pass: &str, cause: Cause) -> PassRecord {
        PassRecord {
            repo: "r".into(),
            batch: batch.into(),
            pass: pass.into(),
            members: vec!["m1".into()],
            instance_type: "c7a.4xlarge".into(),
            vcpus: Some(16),
            place: Place::Ec2,
            capacity_wait_s: 3,
            boot_s: 40,
            build_s: 200,
            suites_s: 500,
            cause,
            detail: String::new(),
        }
    }

    #[test]
    fn a_cause_outside_the_closed_set_is_refused() {
        assert!(Cause::parse("flaky").is_err());
        for c in Cause::ALL {
            assert_eq!(Cause::parse(c.as_str()), Ok(c));
        }
    }

    #[test]
    fn exit_codes_map_to_their_causes() {
        assert_eq!(Cause::from_exit(0, false), Cause::Green);
        assert_eq!(Cause::from_exit(1, false), Cause::RedAttributed);
        assert_eq!(Cause::from_exit(124, false), Cause::OverCap);
        assert_eq!(Cause::from_exit(2, true), Cause::VmLost);
        assert_eq!(Cause::from_exit(3, false), Cause::Infra);
    }

    #[test]
    fn a_record_round_trips_through_its_row() {
        let dir = testkit::TempDir::new("tsd-pass-roundtrip");
        let r = rec("b1", "1", Cause::OverCap);
        r.write(&dir, "h", "2026-10-14T00:00:00Z").unwrap();
        let text = std::fs::read_to_string(crate::family_path(&dir, FAMILY)).unwrap();
        let row: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["verdict"], "incomplete");
        assert_eq!(PassRecord::from_row(&row), Ok(r));
    }

    #[test]
    fn exactly_one_notices_a_missing_and_a_doubled_pass() {
        let dir = testkit::TempDir::new("tsd-pass-count");
        rec("b1", "1", Cause::Green).write(&dir, "h", "2026-10-14T00:00:00Z").unwrap();
        rec("b1", "2", Cause::Green).write(&dir, "h", "2026-10-14T00:00:00Z").unwrap();
        rec("b1", "2", Cause::Green).write(&dir, "h", "2026-10-14T00:00:01Z").unwrap();
        let text = std::fs::read_to_string(crate::family_path(&dir, FAMILY)).unwrap();
        assert!(exactly_one(&text, &[("r", "b1", "1")]).is_ok());
        let e = exactly_one(&text, &[("r", "b1", "1"), ("r", "b1", "2"), ("r", "b1", "3")]).unwrap_err();
        assert!(e.contains("b1/2: 2") && e.contains("b1/3: 0") && !e.contains("b1/1"), "{e}");
    }

    #[test]
    fn from_args_builds_a_forge_batch_record() {
        let a: Vec<String> = "--repo r --batch b --pass 1 --where ec2 --cause vm-lost --vcpus 8 --members a,b".split(' ').map(String::from).collect();
        let r = from_args(&a).unwrap();
        assert_eq!((r.cause, r.vcpus, r.members.len()), (Cause::VmLost, Some(8), 2));
        assert!(from_args(&["--repo".into(), "r".into()]).is_err());
    }
}

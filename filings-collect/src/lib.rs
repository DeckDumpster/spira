//! A product host writes findings as JSON filings under `<home>/filings/outbox`. Each filing
//! becomes one builder bead per (host, class): filed when none is open, appended to when one is.
//! A filing is acknowledged on the host only after its bead holds it, and a filing already
//! applied but not yet acknowledged is acknowledged without being applied again.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub mod real;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq)]
pub struct Host {
    pub name: String,
    pub ssh: String,
    pub home: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Open,
    Append,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Filing {
    pub class: String,
    pub action: Action,
    pub repo: String,
    pub title: String,
    pub body: String,
}

fn safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// `NAME=SSH_TARGET:HOME` entries, whitespace-separated.
pub fn parse_hosts(spec: &str) -> Result<Vec<Host>, String> {
    let mut out: Vec<Host> = Vec::new();
    for e in spec.split_whitespace() {
        let bad = || format!("SPIRA_FILINGS_HOSTS entry {e:?} is not NAME=SSH_TARGET:HOME");
        let (name, rest) = e.split_once('=').ok_or_else(bad)?;
        let (ssh, home) = rest.split_once(':').ok_or_else(bad)?;
        if !safe(name) || ssh.is_empty() || ssh.starts_with('-') || !home.starts_with('/') {
            return Err(bad());
        }
        if out.iter().any(|h| h.name == name) {
            return Err(format!("SPIRA_FILINGS_HOSTS names {name:?} twice"));
        }
        out.push(Host { name: name.into(), ssh: ssh.into(), home: home.into() });
    }
    Ok(out)
}

pub fn parse_filing(json: &str) -> Result<Filing, String> {
    let doc: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let field = |k: &str| -> Result<String, String> {
        doc.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("no `{k}`"))
    };
    let action = match field("action")?.as_str() {
        "open" => Action::Open,
        "append" => Action::Append,
        other => return Err(format!("unknown action {other:?}")),
    };
    let (class, repo) = (field("class")?, field("repo")?);
    if !safe(&class) || !safe(&repo) {
        return Err("`class` and `repo` must be plain names".into());
    }
    Ok(Filing { class, action, repo, title: field("title")?, body: field("body")? })
}

pub fn new_bead_body(host: &Host, name: &str, f: &Filing) -> String {
    let build = if f.body.contains("## Build") {
        String::new()
    } else {
        "\n\n## Build\n\nAct on the findings listed above in this repo, then confirm the producing check reports none of them.".into()
    };
    format!("{}{build}\n\nCollected from {}: {name}\n", f.body.trim_end(), host.name)
}

pub fn append_body(host: &Host, name: &str, f: &Filing) -> String {
    format!("## Filing {name} from {}\n\n{}\n", host.name, f.body.trim_end())
}

pub trait World {
    fn list(&self, h: &Host) -> Result<Vec<String>, String>;
    fn read(&self, h: &Host, name: &str) -> Result<String, String>;
    fn ack(&self, h: &Host, name: &str) -> Result<(), String>;
    /// True when `id` exists and is not in a terminal state.
    fn is_open(&self, id: &str) -> Result<bool, String>;
    fn file(&self, f: &Filing, body: &str) -> Result<String, String>;
    fn append(&self, id: &str, body: &str, note: &str) -> Result<(), String>;
}

/// What the collector remembers between passes: the bead each (host, class) is filed under, and
/// the filings applied to a bead whose acknowledgement has not yet reached the host.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct State {
    pub beads: BTreeMap<String, String>,
    pub applied: BTreeSet<String>,
}

impl State {
    pub fn parse(raw: &str) -> State {
        let mut s = State::default();
        for l in raw.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            match f.as_slice() {
                ["bead", key, id] => {
                    s.beads.insert(key.to_string(), id.to_string());
                }
                ["applied", key] => {
                    s.applied.insert(key.to_string());
                }
                _ => {}
            }
        }
        s
    }

    pub fn render(&self) -> String {
        let beads = self.beads.iter().map(|(k, i)| format!("bead\t{k}\t{i}\n"));
        let applied = self.applied.iter().map(|k| format!("applied\t{k}\n"));
        beads.chain(applied).collect()
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub filed: Vec<String>,
    pub amended: Vec<String>,
    pub acked: Vec<String>,
    /// Each names the host (and filing, when one) it could not handle.
    pub failed: Vec<String>,
}

pub fn pass(
    w: &dyn World,
    hosts: &[Host],
    state: &mut State,
    save: &mut dyn FnMut(&State) -> Result<(), String>,
) -> Outcome {
    let mut o = Outcome::default();
    for h in hosts {
        let mut names = match w.list(h) {
            Ok(n) => n,
            Err(e) => {
                o.failed.push(format!("{}: cannot read the outbox: {e}", h.name));
                continue;
            }
        };
        names.sort();
        for name in names {
            let at = format!("{}/{name}", h.name);
            if !name.ends_with(".json") || !safe(&name) {
                o.failed.push(format!("{at}: not a filing name"));
                continue;
            }
            if !state.applied.contains(&at) {
                let filing = match w.read(h, &name).and_then(|t| parse_filing(&t)) {
                    Ok(f) => f,
                    Err(e) => {
                        o.failed.push(format!("{at}: cannot parse the filing: {e}"));
                        continue;
                    }
                };
                if let Err(e) = apply(w, h, &name, &filing, state, &mut o) {
                    o.failed.push(format!("{at}: {e}"));
                    continue;
                }
                state.applied.insert(at.clone());
                if let Err(e) = save(state) {
                    o.failed.push(format!("{at}: {e}"));
                    continue;
                }
            }
            if let Err(e) = w.ack(h, &name) {
                o.failed.push(format!("{at}: applied but not acknowledged: {e}"));
                continue;
            }
            state.applied.remove(&at);
            o.acked.push(at);
            if let Err(e) = save(state) {
                o.failed.push(e);
            }
        }
    }
    o
}

fn apply(w: &dyn World, h: &Host, name: &str, f: &Filing, state: &mut State, o: &mut Outcome) -> Result<(), String> {
    let key = format!("{}/{}", h.name, f.class);
    let open = match state.beads.get(&key) {
        Some(id) if w.is_open(id)? => Some(id.clone()),
        _ => None,
    };
    match open {
        Some(id) => {
            w.append(&id, &append_body(h, name, f), &format!("filings-collect: {name} from {}", h.name))?;
            o.amended.push(id);
        }
        None => {
            let id = w.file(f, &new_bead_body(h, name, f))?;
            state.beads.insert(key, id.clone());
            o.filed.push(id);
        }
    }
    Ok(())
}

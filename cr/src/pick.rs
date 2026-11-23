//! Weighting, rotation, aim resolution and the seeded draw.

use crate::glob;
use crate::index::{ext, Index};
use crate::ledger::{self, Ledger};
use crate::ports::{Config, Ports, Rng, SourceTree};
use crate::record::{self, State};
use crate::rustsrc;
use crate::units::{Kind, Unit, KINDS};

const DAY_MS: u64 = 86_400_000;
pub const NEVER_REVIEWED_FACTOR: f64 = 4.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Weight {
    pub review_factor: f64,
    /// Days since the latest published review closed; `None` when never reviewed.
    pub days_since_reviewed: Option<f64>,
    pub commits: u32,
    pub dependents: u32,
    pub lines: u32,
    /// Lines changed since each of the unit's files was last reviewed.
    pub changed_lines: u32,
    /// The candidate's area, his comments in it, and his comments in all.
    pub area: String,
    pub area_comments: u32,
    pub his_comments: u32,
    pub total: f64,
}

impl Weight {
    pub fn attention_share(&self) -> f64 {
        if self.his_comments == 0 {
            0.0
        } else {
            f64::from(self.area_comments) / f64::from(self.his_comments)
        }
    }
    pub fn attention_term(&self) -> f64 {
        1.0 + self.attention_share()
    }
    pub fn churn_term(&self) -> f64 {
        1.0 + f64::from(self.commits)
    }
    pub fn dependents_term(&self) -> f64 {
        1.0 + (1.0 + f64::from(self.dependents)).ln()
    }
    pub fn changed_term(&self) -> f64 {
        1.0 + (f64::from(self.changed_lines) / f64::from(self.lines.max(1))).min(1.0)
    }
    pub fn size_term(&self, size_unit_lines: u32) -> f64 {
        (f64::from(self.lines) / f64::from(size_unit_lines.max(1))).min(3.0)
    }
}

#[derive(Debug, Clone)]
pub struct Scored {
    pub unit: Unit,
    pub weight: Weight,
}

pub fn dependent_files(ix: &Index, unit: &Unit) -> Vec<String> {
    let inside: Vec<&str> = unit.files.iter().map(|f| f.path.as_str()).collect();
    let outside = |files: &[String]| files.iter().filter(|f| !inside.contains(&f.as_str())).cloned().collect::<Vec<String>>();
    match unit.kind {
        Kind::Trait => {
            let name = unit.id.rsplit("::").next().unwrap_or("");
            outside(ix.files_naming(name))
        }
        Kind::Module | Kind::Boundary => {
            let target = if unit.kind == Kind::Module { &unit.files[0].path } else { &unit.files[1].path };
            let Some(c) = ix.crate_of(target) else { return Vec::new() };
            let stem = target.rsplit('/').next().unwrap_or("").trim_end_matches(".rs");
            let stem = if stem == "mod" { target.rsplit('/').nth(1).unwrap_or("") } else { stem };
            let mut n: Vec<String> = outside(ix.files_naming_pair(&c.lib, stem));
            let own: Vec<String> = ix.files_naming_pair("crate", stem).iter().filter(|f| f.starts_with(&format!("{}/", c.dir))).cloned().collect();
            n.extend(outside(&own));
            n
        }
        Kind::Script => {
            let bin = unit.id.rsplit(':').next().unwrap_or("");
            ix.text
                .iter()
                .filter(|(p, t)| ext(p) == "sh" && !inside.contains(&p.as_str()) && t.lines().flat_map(crate::units::command_words).any(|w| w == bin))
                .map(|(p, _)| p.clone())
                .collect()
        }
        Kind::Persona => Vec::new(),
    }
}

pub fn dependents(ix: &Index, unit: &Unit) -> u32 {
    dependent_files(ix, unit).len() as u32
}

/// Scores every unit. `ledger` carries today's paths (`Ledger::at_tree`). A unit whose primary
/// file is in flight, or was published at today's blob, is dropped.
pub fn score_all(ix: &Index, tree: &dyn SourceTree, units: Vec<Unit>, cfg: &Config, ledger: &Ledger, now_ms: u64) -> Vec<Scored> {
    let (attention, his_comments) = ledger.attention_by_area();
    let since = now_ms.saturating_sub(u64::from(cfg.churn_days) * DAY_MS);
    units
        .into_iter()
        .filter_map(|unit| {
            let primary = &unit.files[0].path;
            if ledger.in_flight(primary) || ix.blobs.get(primary).is_some_and(|b| ledger.reviewed(primary).is_some_and(|(r, _)| r == *b)) {
                return None;
            }
            let lines: u32 = unit.files.iter().map(|f| ix.lines(&f.path)).sum();
            let commits: u32 = unit.files.iter().map(|f| tree.history(&f.path, since).commits).sum();
            let paths: Vec<&str> = unit.files.iter().map(|f| f.path.as_str()).collect();
            let reviewed: Vec<(String, String, u64)> = paths.iter().filter_map(|p| ledger.reviewed(p).map(|(b, at)| (p.to_string(), b, at))).collect();
            let days = reviewed.iter().map(|(_, _, at)| *at).max().map(|at| now_ms.saturating_sub(at) as f64 / DAY_MS as f64);
            let review_factor = days.map_or(NEVER_REVIEWED_FACTOR, |d| 1.0 + (d / 30.0).min(3.0));
            // A blob the tree cannot diff counts as wholly changed rather than as unchanged.
            let changed_lines: u32 = reviewed.iter().filter(|(p, b, _)| ix.blobs.get(p) != Some(b)).map(|(p, b, _)| tree.changed_lines(b, p).unwrap_or_else(|| ix.lines(p))).sum();
            let mut w = Weight { review_factor, days_since_reviewed: days, commits, dependents: dependents(ix, &unit), lines, changed_lines, area: ledger::area_of(primary).to_string(), area_comments: attention.get(ledger::area_of(primary)).copied().unwrap_or(0), his_comments, total: 0.0 };
            w.total = if paths.iter().any(|p| glob::any(&cfg.exclude, p)) {
                0.0
            } else {
                review_factor * w.attention_term() * w.changed_term() * w.churn_term() * w.dependents_term() * w.size_term(cfg.size_unit_lines)
            };
            Some(Scored { unit, weight: w })
        })
        .collect()
}

/// The forge's ledger at today's paths, and the scored units that are neither in flight nor
/// already staged here.
pub fn candidates(ports: &Ports, ix: &Index, pool: Vec<Unit>, now_ms: u64) -> Result<(Vec<Scored>, Ledger), String> {
    let ledger = ledger::load(ports)?;
    let ledger = ledger.at_tree(ports.tree.as_ref())?;
    let staged: Vec<String> = record::all(&ports.run_dir)?.into_iter().filter(|r| r.state == State::Staged).map(|r| r.unit.id).collect();
    let mut scored = score_all(ix, ports.tree.as_ref(), pool, &ports.config, &ledger, now_ms);
    scored.retain(|s| !staged.contains(&s.unit.id));
    Ok((scored, ledger))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stratum {
    pub kind: Kind,
    pub last_sent_ms: Option<u64>,
    pub skipped: Vec<Kind>,
}

/// Kinds in the order they are tried: never sent first, then oldest last-sent; ties in `KINDS` order.
pub fn kind_order(ledger: &Ledger) -> Vec<(Kind, Option<u64>)> {
    let mut v: Vec<(Kind, Option<u64>)> = KINDS.iter().map(|k| (*k, ledger.last_opened_of_kind(k.as_str()))).collect();
    v.sort_by_key(|(k, last)| (last.map_or((0, 0), |t| (1, t)), *k));
    v
}

pub fn draw(cands: &[Scored], ledger: &Ledger, rng: &mut dyn Rng) -> Option<(usize, Stratum)> {
    let mut skipped = Vec::new();
    for (kind, last) in kind_order(ledger) {
        let idx: Vec<usize> = (0..cands.len()).filter(|i| cands[*i].unit.kind == kind && cands[*i].weight.total > 0.0).collect();
        if idx.is_empty() {
            skipped.push(kind);
            continue;
        }
        let total: f64 = idx.iter().map(|i| cands[*i].weight.total).sum();
        let target = (rng.next_u64() as f64 / u64::MAX as f64) * total;
        let mut acc = 0.0;
        let mut chosen = *idx.last().expect("idx is non-empty");
        for i in &idx {
            acc += cands[*i].weight.total;
            if target < acc {
                chosen = *i;
                break;
            }
        }
        return Some((chosen, Stratum { kind, last_sent_ms: last, skipped }));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Path,
    Crate,
    Identifier,
    Words,
}

impl Resolution {
    pub fn name(&self) -> &'static str {
        match self {
            Resolution::Path => "path",
            Resolution::Crate => "crate",
            Resolution::Identifier => "identifier",
            Resolution::Words => "words",
        }
    }
}

pub struct AimMatch {
    pub resolution: Resolution,
    pub unit_ids: Vec<String>,
}

/// Resolves `aim` in order — path, crate, identifier, words. `Err` carries one line per
/// resolution tried.
pub fn resolve_aim(ix: &Index, units: &[Unit], aim: &str) -> Result<AimMatch, Vec<String>> {
    let mut tried = Vec::new();
    let ids = |f: &dyn Fn(&Unit) -> bool| -> Vec<String> { units.iter().filter(|u| f(u)).map(|u| u.id.clone()).collect() };

    let a = aim.trim_matches('/');
    if ix.has_file(a) {
        let m = ids(&|u| u.files.iter().any(|f| f.path == a));
        if !m.is_empty() {
            return Ok(AimMatch { resolution: Resolution::Path, unit_ids: m });
        }
        tried.push(format!("path: `{a}` is a file at commit {} but no unit contains it", ix.commit));
    } else {
        tried.push(format!("path: no file `{a}` at commit {}", ix.commit));
    }

    if let Some(c) = ix.crate_named(aim) {
        let prefix = format!("{}/", c.dir);
        let m = ids(&|u| u.files.iter().any(|f| f.path.starts_with(&prefix)));
        if !m.is_empty() {
            return Ok(AimMatch { resolution: Resolution::Crate, unit_ids: m });
        }
        tried.push(format!("crate: `{aim}` is a workspace member but no unit lies under {}", c.dir));
    } else {
        tried.push(format!("crate: `{aim}` is not a workspace member name or directory"));
    }

    let mut defs: Vec<(String, &'static str)> = Vec::new();
    for f in ix.files_naming(aim) {
        for line in rustsrc::code_lines(&ix.text[f]) {
            if let Some(kw) = rustsrc::defines_type(&line, aim, false) {
                defs.push((f.clone(), kw));
                break;
            }
        }
    }
    let m = ids(&|u| match u.kind {
        Kind::Trait => defs.iter().any(|(f, kw)| *kw == "trait" && u.files[0].path == *f && u.id.ends_with(&format!("::{aim}"))),
        Kind::Module => defs.iter().any(|(f, kw)| *kw != "trait" && u.files[0].path == *f),
        _ => false,
    });
    if !m.is_empty() {
        return Ok(AimMatch { resolution: Resolution::Identifier, unit_ids: m });
    }
    tried.push(format!("identifier: no `struct|enum|trait|type {aim}` defined at commit {}", ix.commit));

    let words: Vec<String> = aim.split_whitespace().map(str::to_lowercase).collect();
    let m = ids(&|u| {
        let hay = unit_haystack(ix, u);
        !words.is_empty() && words.iter().all(|w| hay.contains(w.as_str()))
    });
    if !m.is_empty() {
        return Ok(AimMatch { resolution: Resolution::Words, unit_ids: m });
    }
    tried.push(format!("words: no unit has every word of {words:?} in its paths or module docs"));
    Err(tried)
}

fn unit_haystack(ix: &Index, u: &Unit) -> String {
    let mut hay = String::new();
    for f in &u.files {
        hay.push_str(&f.path.to_lowercase());
        hay.push('\n');
        if ext(&f.path) == "rs" {
            if let Some(t) = ix.text.get(&f.path) {
                let para: Vec<&str> = t.lines().map(str::trim_start).skip_while(|l| !l.starts_with("//!")).take_while(|l| l.starts_with("//!") && l.trim_start_matches("//!").trim() != "").map(|l| l.trim_start_matches("//!").trim()).collect();
                hay.push_str(&para.join(" ").to_lowercase());
                hay.push('\n');
            }
        }
    }
    hay
}

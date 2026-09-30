//! `lifecycle_enforce` OFF — production today (DESIGN.md §2.9). CHECK 2 and 2c on the
//! records the harness kept before the lifecycle epic (sp-ki12s / sp-mys5p / sp-i2m7y):
//! bd status, assignee and lease, and the protection label. Recovered from sentinel.sh and
//! lib.sh as they stood at f043dee14^ and 542b9445f^, as intent — not a line port. Nothing
//! here calls spira-lc; CHECK 4's legacy half is in check4.rs beside its lifecycle half.

use std::collections::HashSet;

use crate::lifecycle::wait_decisions;
use crate::pass::Sentinel;
use crate::store::{has_all, Snapshot};

/// `bd reclaim`'s success lines (`✓ …` or `Reclaimed …`) → the ids they name
/// (lib.sh `parse_reclaimed`: the first `[a-z]+-[a-z0-9]+` in the line).
pub fn parse_reclaimed(out: &str) -> Vec<Option<String>> {
    out.lines()
        .filter(|l| l.starts_with('✓') || l.starts_with("Reclaimed"))
        .map(first_id)
        .collect()
}

fn first_id(line: &str) -> Option<String> {
    let b = line.as_bytes();
    let lower = |c: u8| c.is_ascii_lowercase();
    let word = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut i = 0;
    while i < b.len() {
        if lower(b[i]) {
            let s = i;
            while i < b.len() && lower(b[i]) {
                i += 1;
            }
            if i + 1 < b.len() && b[i] == b'-' && word(b[i + 1]) {
                let mut e = i + 1;
                while e < b.len() && word(b[e]) {
                    e += 1;
                }
                return Some(line[s..e].to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

/// A lease that has not yet expired (an unparseable one counts as live, as below).
pub fn lease_live(b: &crate::model::Bead, now: i64) -> bool {
    match b.lease_expires_at.as_deref().filter(|l| !l.is_empty()) {
        None => false,
        Some(l) => crate::host::parse_iso(l).map_or(true, |t| t > now),
    }
}

/// CHECK 2c's predicate (lib.sh `orphan_claims`): an OPEN bead in the partition that still
/// carries an assignee and holds no future lease. in_progress is deliberately not here — a
/// live claim is `bd reclaim`'s to time out. An unparseable lease is not evidence of death.
pub fn orphan_claims<'s>(
    snap: &'s Snapshot,
    labels: &[String],
    now: i64,
) -> Vec<(&'s str, &'s str)> {
    snap.list
        .iter()
        .filter(|b| b.status == "open" && has_all(b, labels))
        .filter_map(|b| {
            let who = b
                .assignee
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())?;
            match b.lease_expires_at.as_deref().filter(|l| !l.is_empty()) {
                None => Some((b.id.as_str(), b.assignee.as_deref().unwrap_or(who))),
                Some(l) => match crate::host::parse_iso(l) {
                    Some(t) if t > now => None,
                    Some(_) => Some((b.id.as_str(), b.assignee.as_deref().unwrap_or(who))),
                    None => None,
                },
            }
        })
        .collect()
}

impl<'a> Sentinel<'a> {
    /// CHECK 2 OFF: protect ask-blocked in-progress beads with the skip label, then one
    /// `bd reclaim` per partition that skips the label.
    pub fn check2_legacy(&self, snap: &Snapshot) {
        let ask = self.cfg.ask.clone();
        let skip = self.cfg.reclaim_skip_label.clone();
        let held: HashSet<String> = snap
            .list
            .iter()
            .filter(|b| b.has(&skip))
            .map(|b| b.id.clone())
            .collect();
        let decisions = wait_decisions(snap, &held, &ask);
        let ids: Vec<&str> = decisions.iter().map(|(_, id)| id.as_str()).collect();
        let live = self.reread(&ids);
        for (add, id) in decisions.iter().cloned() {
            let was = snap.get(&id).map(|b| b.status.clone()).unwrap_or_default();
            if self.still("CHECK2", &id, &was, live.as_ref()).is_none() {
                continue;
            }
            if add {
                self.bd().quiet(self.h, &["label", "add", &id, &skip], None);
                self.log(&format!("CHECK2 {id}: only open dep(s) carry {ask} — labeled {skip}, excluded from reclaim"));
                self.act(&format!(
                    "protected {id} from reclaim: waiting on {ask} dep"
                ));
            } else {
                self.bd()
                    .quiet(self.h, &["label", "remove", &id, &skip], None);
                self.log(&format!("CHECK2 {id}: {ask} dep no longer blocking — removed {skip}, re-enters the reaper"));
                self.act(&format!("unprotected {id}: {ask} dep closed"));
            }
        }
        if self.ctx.partitions.is_empty() {
            self.log(
                "CHECK2 no persona in the chamber declares a partition — no lease is being reaped",
            );
            return;
        }
        let older = format!("{}m", self.cfg.reclaim_grace / 60);
        let mut n = 0;
        for p in &self.ctx.partitions {
            let o = self.bd().call(
                self.h,
                &[
                    "reclaim",
                    "--older-than",
                    &older,
                    "--label",
                    &p.labels.join(","),
                    "--exclude-label",
                    &skip,
                ],
                None,
            );
            let out = format!("{}{}", o.stdout, o.stderr);
            if out.contains("No stale leases") {
                continue;
            }
            for id in parse_reclaimed(&out) {
                n += 1;
                if let Some(id) = id {
                    self.write_event_row(&id, "reclaimed", "stale-lease");
                }
            }
        }
        if n > 0 {
            self.progress(&format!("reclaimed {n} stale lease(s)"));
        }
    }

    /// CHECK 2c OFF: release orphaned claims (status open, assignee standing, no lease) so
    /// `bd ready --claim` can take them again. Returns how many moved.
    pub fn check2c_legacy(&self, snap: &Snapshot) -> usize {
        let now = self.h.now();
        let mut seen = HashSet::new();
        let mut orphans = Vec::new();
        for p in &self.ctx.partitions {
            for (id, who) in orphan_claims(snap, &p.labels, now) {
                if seen.insert(id.to_string()) {
                    orphans.push((id, who));
                }
            }
        }
        // `bd assign <id> ""` is unconditional: re-read first, or a claim an aeon took after
        // the snapshot is stripped. Release only a bead that is still the orphan it was.
        let ids: Vec<&str> = orphans.iter().map(|(i, _)| *i).collect();
        let live = self.reread(&ids);
        let mut lines = Vec::new();
        for (id, who) in orphans {
            let Some(b) = self.still("CHECK2c", id, "open", live.as_ref()) else {
                continue;
            };
            if b.assignee.as_deref() != Some(who) || lease_live(b, now) {
                self.log(&format!("CHECK2c {id}: claimed or re-leased since this pass's snapshot — not released"));
                continue;
            }
            if self.bd().quiet(self.h, &["assign", id, ""], None) {
                lines.push(format!("RELEASED\t{id}\t{who}"));
            }
        }
        if !lines.is_empty() {
            self.h.print(&lines.join("\n"));
            self.progress(&format!("released {} orphaned claim(s)", lines.len()));
        }
        lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_output_is_parsed_like_parse_reclaimed() {
        let out = "✓ Reclaimed sp-ab12 (lease expired 4h ago)\nReclaimed sp-cd3 from aeon-x\nsome other line sp-zz\nNo stale leases\n";
        assert_eq!(
            parse_reclaimed(out),
            vec![Some("sp-ab12".to_string()), Some("sp-cd3".to_string())]
        );
        assert_eq!(
            parse_reclaimed("✓ nothing-matchable-? ok"),
            vec![Some("nothing-matchable".to_string())]
        );
        assert_eq!(
            parse_reclaimed("✓ 123"),
            vec![None],
            "counted even when no id is found"
        );
    }

    #[test]
    fn orphan_predicate() {
        let s = Snapshot::from_json(
            r#"[{"id":"o1","status":"open","assignee":"aeon-x","labels":["spira","plan"]},
                {"id":"o2","status":"open","assignee":"aeon-y","lease_expires_at":"2099-01-01T00:00:00Z","labels":["spira","plan"]},
                {"id":"o3","status":"open","assignee":"aeon-z","lease_expires_at":"2000-01-01T00:00:00Z","labels":["spira","plan"]},
                {"id":"o4","status":"open","assignee":"aeon-w","lease_expires_at":"garbage","labels":["spira","plan"]},
                {"id":"o5","status":"in_progress","assignee":"aeon-v","labels":["spira","plan"]},
                {"id":"o6","status":"open","assignee":"  ","labels":["spira","plan"]},
                {"id":"o7","status":"open","assignee":"aeon-u","labels":["spira","incident"]}]"#,
            None,
        );
        let got = orphan_claims(&s, &["spira".into(), "plan".into()], 1_790_000_000);
        assert_eq!(got, vec![("o1", "aeon-x"), ("o3", "aeon-z")]);
    }
}

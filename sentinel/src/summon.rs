//! CHECK 7's inputs, and the --summon-only fast path. The summon loop itself (lanes, pool,
//! fleet ceiling, express grant, elastic reservations) is lib.sh's `ck7_summon_pass`,
//! reached through its seam under the summon.lock it shares with escape.sh (G5).

use crate::cfg::Fayth;
use crate::host::Io;
use crate::cfg::Lifecycle;
use crate::model::{Bead, LcRow};
use crate::pass::Sentinel;
use crate::seams;
use crate::store::{self, has_all, has_none};

/// ready-bucket.py: a bead counts for a fayth iff its labels ⊇ FAYTH_LABELS, are disjoint
/// from FAYTH_EXCLUDE_LABELS and from the shared queue-wait/submitted exclusion, and — when
/// it carries `fayth:<name>` — this fayth is among the named preferences. Fayths with no
/// labels are not rows (bulk_ready_by_fayth skips them).
pub fn bucket(ready: &[Bead], fayths: &[Fayth], shared_exclude: &[String]) -> Vec<(String, usize)> {
    let parts: Vec<&Fayth> = fayths.iter().filter(|f| !f.labels.is_empty()).collect();
    let mut counts: Vec<(String, usize)> = parts.iter().map(|f| (f.name.clone(), 0)).collect();
    for b in ready {
        if !has_none(b, shared_exclude) {
            continue;
        }
        let pref: Vec<&str> = b
            .labels
            .iter()
            .filter_map(|l| l.strip_prefix("fayth:"))
            .collect();
        for (i, f) in parts.iter().enumerate() {
            if !pref.is_empty() && !pref.contains(&f.name.as_str()) {
                continue;
            }
            if has_all(b, &f.labels) && has_none(b, &f.exclude) {
                counts[i].1 += 1;
            }
        }
    }
    counts
}

/// lifecycle_enforce ON: the ready beads a claim could actually take. A bead the lifecycle
/// machine holds on anything but `wait` (a poison, ask or operator hold) is refused by
/// spira-claim's own claim rule (rank.rs `claimable`: `Held`), so CHECK 7 must not count it
/// as ready — else every pass summons an aeon for a poisoned bead that no claim can take.
/// A bead with no lifecycle row is left in (unmigrated; the claim itself decides).
pub fn unheld(ready: &[Bead], rows: &[LcRow]) -> Vec<Bead> {
    let held: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|r| r.holds.iter().any(|h| h != "wait"))
        .map(|r| r.bead_id.as_str())
        .collect();
    ready.iter().filter(|b| !held.contains(b.id.as_str())).cloned().collect()
}

pub fn render_cache(counts: &[(String, usize)]) -> String {
    counts.iter().fold(String::new(), |mut s, (f, n)| {
        use std::fmt::Write;
        let _ = writeln!(s, "{f} {n}");
        s
    })
}

impl<'a> Sentinel<'a> {
    fn shared_exclude(&self) -> Vec<String> {
        [&self.cfg.queue_wait, &self.cfg.submitted]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect()
    }

    /// ready_cache_populate: never hand back an empty-but-existing file, because fayth_ready
    /// trusts the cache unconditionally once it exists.
    ///
    /// ON: held beads are not ready (see [`unheld`]); a machine that cannot answer counts
    /// nothing ready this pass — it is authoritative, and no claim can succeed without it.
    pub fn export_ready_cache(&self, ready: &[Bead]) {
        let ready: Vec<Bead> = match self.lc {
            Lifecycle::Off => ready.to_vec(),
            Lifecycle::On => match self.lc_rows() {
                Some(rows) => unheld(ready, &rows),
                None => Vec::new(),
            },
        };
        let counts = bucket(&ready, &self.ctx.fayths, &self.shared_exclude());
        if counts.is_empty() && !self.ctx.fayths.is_empty() {
            return;
        }
        if let Some(p) = self.temp_file("ready-cache", &render_cache(&counts)) {
            self.h.set_env("SPIRA_READY_CACHE", &p.to_string_lossy());
        }
    }

    /// --summon-only: the gate, the live count, ONE bd ready, then CHECK 7.
    pub fn summon_only(&self) -> i32 {
        let g = self.seam(
            "summon-gate",
            seams::SUMMON_GATE,
            None,
            Io::Inherit,
            Io::Inherit,
            true,
        );
        if !g.ok() {
            if g.rc == 97 {
                self.log("summon-only: lib.sh did not load — not summoning");
            }
            return 0;
        }
        let live = self.live_total();
        self.log(&format!(
            "summon-only: live={live} fayths=[{}]",
            self.cfg.fayths_str
        ));
        if let Ok((_, ready)) = store::read_json(&self.bd(), self.h, &store::ready_args(&self.cfg))
        {
            self.export_ready_cache(&ready);
        }
        self.seam("ck7", seams::CK7, None, Io::Inherit, Io::Inherit, true);
        crate::temps::cleanup();
        self.h.unset_env("SPIRA_READY_CACHE");
        self.log(&format!(
            "summon-only pass complete — {} action(s)",
            self.acted.get()
        ));
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    fn f(n: &str, l: &str, e: &str) -> Fayth {
        Fayth {
            name: n.into(),
            labels: crate::cfg::csv(l),
            exclude: crate::cfg::csv(e),
        }
    }

    #[test]
    fn bucket_is_ready_bucket_py() {
        let ready = parse_beads(
            r#"[{"id":"1","labels":["spira","plan"]},
                {"id":"2","labels":["spira","plan","fayth:ops"]},
                {"id":"3","labels":["spira","incident"]},
                {"id":"4","labels":["spira","plan","spira-queue-waiting"]},
                {"id":"5","labels":["spira","plan","qa-proposed"]}]"#,
        )
        .unwrap();
        let fs = vec![
            f("builder", "spira,plan", "qa-proposed"),
            f("ops", "spira,incident", ""),
            f("spike", "spira", ""),
            f("concierge", "", ""),
        ];
        let c = bucket(
            &ready,
            &fs,
            &["spira-queue-waiting".into(), "spira-submitted".into()],
        );
        assert_eq!(
            c,
            vec![
                ("builder".into(), 1),
                ("ops".into(), 1),
                ("spike".into(), 3)
            ]
        );
        assert_eq!(render_cache(&c), "builder 1\nops 1\nspike 3\n");
    }

    fn row(id: &str, holds: &[&str]) -> LcRow {
        LcRow {
            bead_id: id.into(),
            state: "READY".into(),
            holds: holds.iter().map(|h| h.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn unheld_drops_poison_ask_and_operator_holds_but_keeps_wait_and_rowless() {
        let ready = parse_beads(
            r#"[{"id":"p","labels":["spira","plan"]},
                {"id":"a","labels":["spira","plan"]},
                {"id":"o","labels":["spira","plan"]},
                {"id":"w","labels":["spira","plan"]},
                {"id":"free","labels":["spira","plan"]},
                {"id":"norow","labels":["spira","plan"]}]"#,
        )
        .unwrap();
        let rows = vec![
            row("p", &["poison"]),
            row("a", &["ask"]),
            row("o", &["operator", "wait"]),
            row("w", &["wait"]),
            row("free", &[]),
        ];
        let ids: Vec<String> = unheld(&ready, &rows).into_iter().map(|b| b.id).collect();
        assert_eq!(ids, vec!["w", "free", "norow"]);
        let c = bucket(&unheld(&ready, &rows), &[f("builder", "spira,plan", "")], &[]);
        assert_eq!(c, vec![("builder".into(), 3)]);
    }
}

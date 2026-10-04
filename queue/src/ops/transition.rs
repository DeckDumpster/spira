//! Land-mode transitions: to-forge, to-local (DESIGN.md §2.2, §8 D5, D6).

use super::{idents, repo_path, resolve, take_lock, Ctx, World, FAIL, OK};
use crate::model::LandMode;
use crate::ports::{ref_branch, Emit};
use crate::records;

/// The legacy map (what lib.sh's readers see) and the config (spira-config's document) must
/// already agree about the row before either transition touches anything (queue.sh
/// _land_mode_agrees). No config document in force: nothing to disagree with.
fn agrees(w: &World, c: &Ctx) -> Result<(), i32> {
    let Some(doc) = w.lib.toml_path() else { return Ok(()) };
    let (cf_mode, cf_base) = match w.config.repo_row(&doc, &c.r.name) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh: cannot read {}: {e} — refused", doc.display()));
            return Err(FAIL);
        }
    };
    let cf_mode = if cf_mode == "queue.forge" { "queue".to_string() } else { cf_mode };
    let map_land = c.r.mode.as_str();
    if map_land != cf_mode || c.r.map_base != cf_base {
        let map = c.s.repo_map.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "<legacy map>".into());
        w.err(format!(
            "queue.sh: {map} and {} already disagree about {} ({map}: {map_land}|{}; {}: {cf_mode}|{cf_base}) — refused, reconcile by hand first",
            doc.display(),
            c.r.name,
            c.r.map_base,
            doc.display()
        ));
        return Err(FAIL);
    }
    Ok(())
}

/// The work of this repository that is between CERTIFIED-into-a-round and LANDED (§8 D5): an
/// open batch record plus every IN_DELIVERY lifecycle row whose tip is a commit here. A
/// failed spira-lc read is Err (cannot tell), which refuses like a positive answer.
pub fn in_delivery(w: &World, c: &Ctx, path: &std::path::Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if let Ok(Some(kv)) = records::read_kv(&c.queue_file("open")) {
        out.push(format!("open batch PR {}", kv.get_first("pr").unwrap_or("?")));
    }
    let here = |t: &str| crate::ident::check("tip", t).is_ok() && w.git.commit_exists(path, t);
    for row in w.lc.bead_rows(Some("IN_DELIVERY"))? {
        if row.tip.as_deref().map(here).unwrap_or(false) && !out.contains(&row.bead_id) {
            out.push(row.bead_id.clone());
        }
    }
    Ok(out)
}

fn refuse_in_delivery(w: &World, label: &str, c: &Ctx, path: &std::path::Path) -> Result<(), i32> {
    super::require_lc(w, label)?;
    match in_delivery(w, c, path) {
        Ok(v) if v.is_empty() => Ok(()),
        Ok(v) => {
            w.err(format!(
                "queue.sh {label}: {} has work in delivery ({}) — refused, nothing changed; let it land or abandon it first",
                c.r.name,
                v.join(", ")
            ));
            Err(FAIL)
        }
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot tell whether {} has work in delivery ({e}) — refused, nothing changed", c.r.name));
            Err(FAIL)
        }
    }
}

/// Write the land-mode pair, both through spira-config's library (the only door onto
/// either file): the config document first (validated, atomic, both keys at once), then the
/// legacy map's row while that file exists (§8 D6). A legacy-map failure restores the
/// document's previous row, so the two are never left disagreeing by this call.
fn write_row(w: &World, c: &Ctx, mode: LandMode, base: &str) -> Result<(), i32> {
    let name = &c.r.name;
    let doc = w.lib.toml_path();
    let mut previous = None;
    if let Some(d) = &doc {
        previous = w.config.repo_row(d, name).ok();
        if let Err(e) = w.config.set_repo_row(d, name, mode.config_word(), base) {
            w.err(format!("queue.sh: setting {name} to {}|{base} in {} failed ({e}) — nothing changed", mode.config_word(), d.display()));
            return Err(FAIL);
        }
    }
    let Some(map) = c.s.repo_map.clone().filter(|p| p.is_file()) else { return Ok(()) };
    match w.config.set_legacy_map_row(&map, name, mode.config_word(), base) {
        Ok(()) => Ok(()),
        Err(e) => {
            w.err(format!("queue.sh: {e} — refused"));
            if let (Some(d), Some((m, b))) = (&doc, &previous) {
                if w.config.set_repo_row(d, name, m, b).is_err() {
                    w.err(format!("queue.sh: could not restore {name}'s row in {} to {m}|{b} — the two disagree, fix by hand", d.display()));
                }
            }
            Err(FAIL)
        }
    }
}

/// Routes stderr to stdout (the captured-then-printed final publish).
struct Merged<'a>(&'a dyn Emit);
impl Emit for Merged<'_> {
    fn out(&self, s: &str) {
        self.0.out(s)
    }
    fn err(&self, s: &str) {
        self.0.out(s)
    }
}

pub fn to_forge(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "to-forge", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "to-forge", &c) else { return FAIL };
    let name = c.r.name.clone();
    if c.r.mode != LandMode::QueueLocal {
        w.err(format!("queue.sh to-forge: {name} is not in queue.local mode (mode={}) — nothing to transition", c.r.mode.as_str()));
        return FAIL;
    }
    if agrees(w, &c).is_err() {
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh to-forge: cannot resolve landing ref for {name}"));
        return FAIL;
    };
    if c.r.ref_remote(&base).is_some() {
        w.err(format!("queue.sh to-forge: {name} resolves to a remote-tracking ref ({base}) — not a queue.local base"));
        return FAIL;
    }
    if idents(w, "to-forge", &[("base", &base)]).is_err() {
        return FAIL;
    }
    if !w.git.ref_exists(&path, &format!("refs/heads/{base}")) {
        w.err(format!("queue.sh to-forge: {base} does not exist as a local branch in {}", path.display()));
        return FAIL;
    }
    if w.git.current_branch(&path).as_deref() == Some(base.as_str()) {
        w.err(format!("queue.sh to-forge: the checkout at {} is on {base} — check out a different branch before repointing it", path.display()));
        return FAIL;
    }
    let Some((remote, fbranch)) = c.r.publish.clone() else {
        w.err(format!("queue.sh to-forge: cannot resolve a forge target for {name}"));
        return FAIL;
    };
    let Ok(_g) = take_lock(w, "to-forge", &c, " — stop cutting local rounds first") else { return FAIL };

    // Re-read under the lock.
    let Ok(c) = resolve(w, "to-forge", Some(&name)) else { return FAIL };
    if c.r.mode != LandMode::QueueLocal {
        w.err(format!("queue.sh to-forge: {name} is not in queue.local mode (mode={}) — nothing to transition", c.r.mode.as_str()));
        return FAIL;
    }
    if refuse_in_delivery(w, "to-forge", &c, &path).is_err() {
        return FAIL;
    }

    w.out(format!("queue.sh to-forge: running the final publish for {name}"));
    let merged = Merged(w.io);
    let w2 = World { io: &merged, ..*w };
    if super::publish::publish_with(&w2, Some(&name), true) != 0 {
        w.err("queue.sh to-forge: the final publish failed — refused, nothing changed");
        return FAIL;
    }

    let pfile = c.queue_file("publish");
    if pfile.exists() {
        let pr = records::read_kv(&pfile).ok().flatten().and_then(|k| k.get("pr").map(String::from)).unwrap_or_default();
        let deadline = w.clock.now() + c.s.transition_maxsec;
        w.out(format!("queue.sh to-forge: waiting for publish PR {pr} to settle green"));
        loop {
            if super::verdict::settle_publish(w, &c, &path) == super::verdict::RED {
                w.err(format!(
                    "queue.sh to-forge: the final publish (PR {pr}) is red — refused, nothing changed; it is left open for the normal fix-forward recovery"
                ));
                return FAIL;
            }
            if !pfile.exists() {
                break;
            }
            if w.clock.now() >= deadline {
                w.err(format!("queue.sh to-forge: timed out waiting for publish PR {pr} to settle — refused, nothing changed"));
                return FAIL;
            }
            w.clock.sleep(c.s.transition_pollsec);
        }
    }

    if !w.git.fetch(&path, &remote, &fbranch) {
        w.err(format!("queue.sh to-forge: could not fetch {remote}/{fbranch} to verify"));
        return FAIL;
    }
    let forge_sha = w.git.rev_parse(&path, &format!("refs/remotes/{remote}/{fbranch}"));
    let local_sha = w.git.rev_parse(&path, &base);
    let (Some(fs_), Some(ls_)) = (forge_sha.clone(), local_sha.clone()) else {
        w.err(format!(
            "queue.sh to-forge: {remote}/{fbranch} ({}) and {base} ({}) differ — refused, nothing changed",
            forge_sha.unwrap_or_else(|| "<none>".into()),
            local_sha.unwrap_or_else(|| "<none>".into())
        ));
        return FAIL;
    };
    if fs_ != ls_ {
        w.err(format!("queue.sh to-forge: {remote}/{fbranch} ({fs_}) and {base} ({ls_}) differ — refused, nothing changed"));
        return FAIL;
    }

    let new_base = format!("{remote}/{fbranch}");
    if write_row(w, &c, LandMode::Queue, &new_base).is_err() {
        w.err(format!("queue.sh to-forge: writing the new row failed for {name} — reconcile the config and the legacy map by hand"));
        return FAIL;
    }
    let (m, r) = w.lib.readback(&name);
    if m != "queue" || r != new_base {
        w.err(format!(
            "queue.sh to-forge: post-write verification failed for {name} (mode={m} ref={r}, expected queue at {new_base}) — fix by hand"
        ));
        return FAIL;
    }
    let archive = format!("refs/archive/{base}");
    let _ = w.git.update_ref(&path, &archive, &ls_, None);
    let _ = w.git.branch_delete(&path, &base);
    w.lib.notify(
        &name,
        "flipped to queue.forge",
        &format!("{name} moved from queue.local to queue.forge; base is now {new_base}. {base} archived at {archive}."),
    );
    w.out(format!("queue.sh to-forge: {name} is now queue.forge (base={new_base}); {base} archived at {archive}"));
    OK
}

pub fn to_local(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "to-local", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "to-local", &c) else { return FAIL };
    let name = c.r.name.clone();
    if c.r.mode != LandMode::Queue {
        w.err(format!("queue.sh to-local: {name} is not in queue.forge mode (mode={}) — nothing to transition", c.r.mode.as_str()));
        return FAIL;
    }
    if agrees(w, &c).is_err() {
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh to-local: cannot resolve landing ref for {name}"));
        return FAIL;
    };
    let Some(remote) = c.r.ref_remote(&base) else {
        w.err(format!("queue.sh to-local: {base} does not resolve to a remote-tracking ref — not a queue.forge base"));
        return FAIL;
    };
    let fbranch = ref_branch(&base).to_string();
    let new_base = format!("local/{fbranch}");
    if idents(w, "to-local", &[("base", &base), ("new base", &new_base)]).is_err() {
        return FAIL;
    }
    if w.git.current_branch(&path).as_deref() == Some(new_base.as_str()) {
        w.err(format!("queue.sh to-local: the checkout at {} is already on {new_base} — check out a different branch first", path.display()));
        return FAIL;
    }
    if w.git.ref_exists(&path, &format!("refs/heads/{new_base}")) {
        w.err(format!("queue.sh to-local: {new_base} already exists as a local branch — refused, nothing changed"));
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "to-local", &c, "") else { return FAIL };
    let Ok(c) = resolve(w, "to-local", Some(&name)) else { return FAIL };
    if c.r.mode != LandMode::Queue {
        w.err(format!("queue.sh to-local: {name} is not in queue.forge mode (mode={}) — nothing to transition", c.r.mode.as_str()));
        return FAIL;
    }
    if refuse_in_delivery(w, "to-local", &c, &path).is_err() {
        return FAIL;
    }
    if !w.git.fetch(&path, &remote, &fbranch) {
        w.err(format!("queue.sh to-local: could not fetch {remote}/{fbranch}"));
        return FAIL;
    }
    let Some(forge_sha) = w.git.rev_parse(&path, &format!("refs/remotes/{remote}/{fbranch}")) else {
        w.err(format!("queue.sh to-local: cannot resolve {remote}/{fbranch}"));
        return FAIL;
    };
    let archive = format!("refs/archive/{new_base}");
    if let Some(start) = w.git.rev_parse(&path, &archive) {
        if !w.git.is_ancestor(&path, &start, &forge_sha) {
            w.err(format!(
                "queue.sh to-local: the archived {new_base} ({start}) is not an ancestor of {remote}/{fbranch} ({forge_sha}) — refs differ, refused"
            ));
            return FAIL;
        }
    }
    if !w.git.branch_set(&path, &new_base, &forge_sha, false) {
        w.err(format!("queue.sh to-local: could not create {new_base} at {forge_sha}"));
        return FAIL;
    }
    if write_row(w, &c, LandMode::QueueLocal, &new_base).is_err() {
        let _ = w.git.branch_delete(&path, &new_base);
        w.err(format!("queue.sh to-local: writing the new row failed for {name} — reconcile the config and the legacy map by hand"));
        return FAIL;
    }
    let (m, r) = w.lib.readback(&name);
    if m != "queue.local" || r != new_base {
        w.err(format!(
            "queue.sh to-local: post-write verification failed for {name} (mode={m} ref={r}, expected queue.local at {new_base}) — fix by hand"
        ));
        return FAIL;
    }
    w.lib.notify(
        &name,
        "flipped to queue.local",
        &format!("{name} moved from queue.forge to queue.local; base is now {new_base}, synced to {remote}/{fbranch} at {forge_sha}."),
    );
    w.out(format!("queue.sh to-local: {name} is now queue.local (base={new_base}, synced to {remote}/{fbranch})"));
    OK
}

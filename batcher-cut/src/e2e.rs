//! The end-to-end proof (DESIGN.md §4.4), `#[ignore]`d because it runs for ~15 s:
//!
//!   cargo test -p batcher-cut --bin batcher e2e -- --ignored --nocapture
//!
//! A real fixture repository with three members, one of which breaks a fast suite; real git
//! merges for every rerun tree; the real `VmRunner` and spool protocol; and a stub round-vm
//! that runs the fixture suites on this box instead of a VM (only the VM is faked). It shows
//! attribution settling before the corpus ends and the right member ejected, with timings.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use batcher::attrib::{Budget, Decision};
use batcher::core::Member;

use crate::drive::{attribute_round, RoundEnd, RoundOps};
use crate::io::{Env, Land, Repo};

/// Stands in for `round-vm run <wt> ... --results-dir R --attr-spool S`: runs the corpus
/// `maxpar`-wide from the round worktree, writing each `.result` as the suite finishes, and
/// serves spool requests concurrently from a detached worktree of the requested branch.
const STUB: &str = r#"#!/usr/bin/env bash
set -u
shift
wt="$1"; shift
while [ $# -gt 0 ]; do
    case "$1" in
        --suites) suites="$2" ;; --maxpar) maxpar="$2" ;; --results-dir) res="$2" ;; --attr-spool) spool="$2" ;;
    esac
    shift 2
done
runone() {
    local d="$1" s="$2" o="$3" t0 rc st
    t0=$(date +%s)
    ( cd "$d" && bash "spira/$s" ) > "$o/$s.out" 2>&1; rc=$?
    if [ "$rc" = 0 ]; then st=ok; else st=red; fi
    printf '%s %s %s - parallel explicit %s\n' "$st" "$t0" "$(( $(date +%s) - t0 ))" "$rc" > "$o/.$s.tmp"
    mv "$o/.$s.tmp" "$o/$s.result"
}
export -f runone
serve() {
    declare -A taken
    while :; do
        for r in "$spool"/req/*.req; do
            [ -e "$r" ] || continue
            j="$(basename "$r" .req)"
            [ -n "${taken[$j]:-}" ] && continue
            build="$(sed -n 's/^build=//p' "$r")"
            [ "$build" = round ] && [ ! -e "$spool/corpus.done" ] && continue
            taken[$j]=1
            (
                br="$(sed -n 's/^branch=//p' "$r")"; s="$(sed -n 's/^suites=//p' "$r")"
                d="$(mktemp -d)"; rmdir "$d"
                git -C "$wt" worktree add -q --detach "$d" "$br"
                mkdir -p "$spool/res/$j"; rc=0
                for x in ${s//,/ }; do
                    runone "$d" "$x" "$spool/res/$j"
                    grep -q '^ok' "$spool/res/$j/$x.result" || rc=1
                done
                if [ "$build" = round ]; then
                    printf 'tree=%s\n' "$(git -C "$d" rev-parse 'HEAD^{tree}')" > "$spool/res/$j/batch.meta"
                    mkdir -p "$spool/res/$j/bins"; printf 'x\n' > "$spool/res/$j/bins/fakebin"; chmod +x "$spool/res/$j/bins/fakebin"
                fi
                git -C "$wt" worktree remove -f "$d"
                printf 'rc=%s\n' "$rc" > "$spool/res/.$j.tmp"; mv "$spool/res/.$j.tmp" "$spool/res/$j.done"
                printf '%s %s %s\n' "$(date +%s.%N)" "$j" "$build" >> "$spool/../stub.log"
            ) &
        done
        [ -e "$spool/close" ] && break
        sleep 0.1
    done
    wait
}
serve & srv=$!
export wt res
printf '%s\n' ${suites//,/ } | xargs -P "$maxpar" -I{} bash -c 'runone "$wt" {} "$res"'
rc=0; grep -q '^red' "$res"/*.result && rc=1
printf 'corpus-end %s\n' "$(date +%s.%N)" >> "$spool/../stub.log"
printf 'rc=%s\n' "$rc" > "$spool/.corpus.tmp"; mv "$spool/.corpus.tmp" "$spool/corpus.done"
wait "$srv"
exit "$rc"
"#;

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write(p: &Path, body: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn commit(repo: &Path, msg: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]);
}

struct ProofOps {
    wt: PathBuf,
    env: Env,
    repo: Repo,
    start: String,
    changed: BTreeMap<String, Vec<String>>,
    ejected: Vec<(String, Vec<String>)>,
    decisions: Vec<(u32, Decision)>,
}

impl RoundOps for ProofOps {
    fn suspects(&self, suite: &str, members: &[String]) -> Vec<String> {
        crate::suspects_in(&self.wt, &self.changed, suite, members)
    }
    fn eject(&mut self, member: &Member, suites: &[String], _owner: bool) {
        self.ejected.push((member.id.clone(), suites.to_vec()));
    }
    fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
        crate::merge_round(&self.env, &self.repo, &self.wt, &self.start, survivors.to_vec())
    }
    fn fix_integration(&mut self, members: &[Member], _: &[String]) -> bool {
        crate::io::integration_fix(&self.env, &self.wt, &self.start, members).map(|f| !f.is_empty()).unwrap_or(false)
    }
    fn base_moved(&mut self, _: &[Member]) -> Result<Option<(String, Vec<Member>)>, String> {
        Ok(None)
    }
    fn judge(&mut self, suites: &[String], _: &[String]) {
        panic!("no judgement expected: {suites:?}");
    }
    fn incident(&mut self, kind: &str, suites: &[String]) {
        panic!("no incident expected: {kind} {suites:?}");
    }
    fn touching(&self, _suite: &str, _members: &[String]) -> Vec<String> {
        vec![]
    }
    fn delete_flips(&mut self, suites: &[String]) -> Result<(), String> {
        panic!("no flip expected: {suites:?}");
    }
    fn record(&mut self, iteration: u32, d: &Decision) {
        self.decisions.push((iteration, d.clone()));
    }
    fn escape(&mut self, _member: &Member, _suite: &str, _rerun: Option<batcher::attrib::JobResult>) {}
}

#[test]
#[ignore]
fn e2e_three_members_one_breaks_a_fast_suite() {
    let root = testkit::TempDir::new("batcher-e2e");
    let repo_dir = root.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init", "-q", "-b", "main"]);
    write(&repo_dir.join("spira/test-fast.sh"), "#!/bin/bash\n# covers: spira/widget.sh\n. spira/widget.sh\nsleep 0.3\n[ \"$(widget)\" = ok ] || { echo 'FAIL widget'; exit 1; }\n");
    write(&repo_dir.join("spira/widget.sh"), "widget() { echo ok; }\n");
    for i in 1..=8 {
        write(&repo_dir.join(format!("spira/test-slow-{i}.sh")), "#!/bin/bash\n# covers: spira/other.sh\nsleep 3\n");
    }
    write(&repo_dir.join("spira/other.sh"), "true\n");
    commit(&repo_dir, "base");
    let base = git(&repo_dir, &["rev-parse", "HEAD"]);
    let mut members = vec![];
    for (id, path, body) in [
        ("sp-m1", "doc/one.md", "one\n"),
        ("sp-m2", "spira/widget.sh", "widget() { echo broken; }\n"),
        ("sp-m3", "spira/three.sh", "true\n"),
    ] {
        git(&repo_dir, &["checkout", "-q", "-b", &format!("spira/{id}"), &base]);
        write(&repo_dir.join(path), body);
        commit(&repo_dir, id);
        let tip = git(&repo_dir, &["rev-parse", "HEAD"]);
        members.push(Member { id: id.into(), tip, title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new() });
    }
    git(&repo_dir, &["checkout", "-q", "main"]);

    let stub = root.join("round-vm-stub");
    testkit::write_exe(&stub, STUB);
    let run = root.join("run");
    let env = Env {
        home: root.join("no-home"),
        run: run.clone(),
        queue_dir: run.join("queue"),
        db: None,
        bd: "bd".into(),
        express_label: "express".into(),
        forge: PathBuf::new(),
        tsd_bin: None,
        round_vm: stub,
        queue_bin: root.join("queue"),
        rebase_stale_bin: root.join("rebase-stale"),
        round_slots: Some(4),
        poll_secs: 1,
        maxpar: 2,
        wall_secs: 600,
        rust_toolchain: "1.82.0".into(),
        git_name: "t".into(),
        git_email: "t@t".into(),
        lc_bin: None,
        lc_timeout: 5,
        verdicts: run.join("verdicts"),
        land_lock_attempts: 3,
        land_lock_wait: std::time::Duration::from_millis(1),
    };
    let repo = Repo { name: "fx".into(), path: repo_dir.clone(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
    let wt = run.join("worktree/.batcher-fx");

    let t0 = Instant::now();
    let merged = crate::merge_round(&env, &repo, &wt, &base, members.clone()).unwrap();
    assert_eq!(merged.len(), 3);
    let branch = "spira/batcher-attr/fx-proof";
    crate::io::set_branch(&repo, branch, &crate::io::head_of(&wt).unwrap());
    let suites = crate::io::all_suites(&repo, branch);
    let changed: BTreeMap<String, Vec<String>> = merged.iter().map(|m| (m.id.clone(), crate::io::changed_paths(&repo, &base, &m.tip))).collect();
    let mut runner = crate::vm::VmRunner::new(&env, &repo, &wt, &base, "proof", changed.clone()).unwrap();
    let log = runner.spool.parent().unwrap().join("stub.log");
    let mut ops = ProofOps { wt: wt.clone(), env: env.clone(), repo: repo.clone(), start: base.clone(), changed, ejected: vec![], decisions: vec![] };
    let started = crate::now();
    let end = attribute_round(&mut runner, &mut ops, &suites, merged, Budget { slots: 4, maxpar: 2 }).unwrap();
    runner.close();
    let wall = t0.elapsed().as_secs_f64();

    let d0 = &ops.decisions[0].1;
    let rec = &d0.records[0];
    println!("\n=== e2e proof: 3 members, sp-m2 breaks test-fast.sh; corpus = 9 suites at maxpar 2, round slots 4 ===");
    println!("stub log (epoch, job, build):\n{}", fs::read_to_string(&log).unwrap_or_default());
    println!(
        "red streamed at +{}s; attributed ({:?}) at +{}s after {} rerun(s); settled before corpus end: {}",
        rec.red_at - started,
        rec.outcome,
        rec.settled_at.unwrap() - started,
        rec.reruns,
        rec.settled_before_main_end
    );
    println!("ejected: {:?}", ops.ejected);
    println!("round end: {:?}", end.clone_members());
    println!("whole round wall (merge → corpus → verification → decision): {wall:.1}s");

    assert_eq!(ops.ejected, vec![("sp-m2".to_string(), vec!["test-fast.sh".to_string()])]);
    assert!(rec.settled_before_main_end, "attribution finished before the corpus ended");
    assert_eq!(ops.decisions.len(), 2, "corpus, then the survivors' verification of test-fast.sh alone");
    assert!(ops.decisions[1].1.records.is_empty(), "test-fast.sh green on the survivors");
    match end {
        RoundEnd::Land { members, .. } => assert_eq!(members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["sp-m1", "sp-m3"]),
        other => panic!("expected to land, got {other:?}"),
    }
    // path-ok: a test asserting where the survivors' fixture binary is installed in a temp worktree
    assert!(wt.join("target/release/fakebin").is_file(), "the survivors' binaries were installed");
    let _ = fs::remove_dir_all(&root);
}

trait CloneMembers {
    fn clone_members(&self) -> String;
}
impl CloneMembers for RoundEnd {
    fn clone_members(&self) -> String {
        match self {
            RoundEnd::Land { members, attribution_secs } => {
                format!("land {:?} (attribution {:?}s)", members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), attribution_secs)
            }
            RoundEnd::Blocked(w) => format!("blocked: {w}"),
        }
    }
}

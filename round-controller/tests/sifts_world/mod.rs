//! A recorded pool for the Sift: one queue.local repository whose beads take every verdict the
//! screen gives, a stub `spira-lc` answering from files and logging every write, and a stub
//! `bead.sh` logging every note. Shared by the controller's own tests and the batcher parity test.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use round_controller::sifts::Env;
use spira_config::repos::Registry;

/// The recorded epoch every run of the world is stamped with.
pub const NOW: &str = "1700000000";

const LC_STUB: &str = r#"#!/bin/bash
d="$(cd "$(dirname "$0")/.." && pwd)"
case "$1 $2" in
  "list --state") cat "$d/lc/rows.json" ;;
  "list --ids") printf '[]' ;;
  "show "*) [ -f "$d/lc/show/$2.json" ] || { echo "no bead $2" >&2; exit 1; }; cat "$d/lc/show/$2.json" ;;
  "plan items")
    case "$3 $4" in
      "--full --ids")
        printf '{"watermark":0,"items":['; sep=''
        for i in ${5//,/ }; do
          [ -f "$d/lc/content/$i.json" ] && { printf '%s' "$sep"; cat "$d/lc/content/$i.json"; sep=','; }
        done
        printf ']}' ;;
      "--external-contains "*) printf '{"watermark":0,"items":[]}' ;;
      *) echo "stub spira-lc: unexpected $*" >&2; exit 2 ;;
    esac ;;
  "event bead") printf 'spira-lc %s\n' "$*" >> "$d/effects.log"; printf '{}' ;;
  "close "*) printf 'spira-lc %s <<< %s\n' "$*" "$(/bin/cat)" >> "$d/effects.log" ;;
  *) echo "stub spira-lc: unexpected $*" >&2; exit 2 ;;
esac
"#;

const BEAD_STUB: &str = r#"#!/bin/bash
d="$(cd "$(dirname "$0")/.." && pwd)"
printf 'bead.sh %s\n' "$*" >> "$d/effects.log"
"#;

pub struct World {
    pub root: PathBuf,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub tips: BTreeMap<String, String>,
}

fn git(dir: &Path, args: &[&str]) -> String {
    testkit::git(dir, args)
}

fn write(p: &Path, body: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

impl World {
    pub fn build(root: &Path) -> World {
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "local/main"]);
        // The screen's own trial merge commits here; the repository carries the identity, so
        // the world screens the same without a user git config (the unit sandbox has none).
        git(&repo, &["config", "user.name", "sift-world"]);
        git(&repo, &["config", "user.email", "sift-world@t.invalid"]);
        write(&repo.join("base.txt"), "one\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "root"]);
        let mut tips = BTreeMap::new();
        let mut branch = |id: &str, from: &str, commits: &[(&str, &str, &str)]| {
            git(
                &repo,
                &["checkout", "-q", "-b", &format!("spira/{id}"), from],
            );
            for (file, body, subject) in commits {
                write(&repo.join(file), body);
                git(&repo, &["add", "-A"]);
                git(&repo, &["commit", "-qm", subject]);
            }
            tips.insert(id.to_string(), git(&repo, &["rev-parse", "HEAD"]));
        };
        branch(
            "sp-clean",
            "local/main",
            &[("clean.txt", "clean\n", "sp-clean: adds clean")],
        );
        branch(
            "sp-conflict",
            "local/main",
            &[("base.txt", "mine\n", "sp-conflict: edits base")],
        );
        branch(
            "sp-dupe",
            "local/main",
            &[("dupe.txt", "same\n", "sp-dupe: copies")],
        );
        branch(
            "sp-inround",
            "local/main",
            &[("dupe.txt", "same\n", "sp-inround: the original")],
        );
        branch(
            "sp-broken",
            "local/main",
            &[("broken.txt", "b\n", "sp-broken: part one")],
        );
        branch(
            "sp-stacked",
            "spira/sp-broken",
            &[("stacked.txt", "s\n", "sp-stacked: on broken")],
        );
        branch(
            "sp-capped",
            "spira/sp-broken",
            &[("capped.txt", "c\n", "sp-capped: on broken")],
        );
        branch(
            "sp-seen",
            "local/main",
            &[("seen.txt", "seen\n", "sp-seen: already passed")],
        );
        branch(
            "sp-gone",
            "local/main",
            &[("gone.txt", "gone\n", "sp-gone: dropped since listing")],
        );
        branch(
            "sp-moved",
            "local/main",
            &[("moved.txt", "moved\n", "sp-moved: branch moved on")],
        );
        branch(
            "sp-held",
            "local/main",
            &[("held.txt", "held\n", "sp-held: held")],
        );
        branch(
            "sp-onheld",
            "local/main",
            &[("onheld.txt", "on\n", "sp-onheld: stacked on held")],
        );
        branch(
            "sp-ident",
            "local/main",
            &[("ui.txt", "say Zorblax\n", "sp-ident: borrows a word")],
        );
        branch(
            "sp-target",
            "local/main",
            &[("target.txt", "t\n", "sp-target: declares a target")],
        );
        branch(
            "sp-nocontent",
            "local/main",
            &[("nc.txt", "nc\n", "sp-nocontent: no content row")],
        );
        branch(
            "sp-ejected",
            "local/main",
            &[("ejected.txt", "e\n", "sp-ejected: ejected red at this tip")],
        );
        git(&repo, &["checkout", "-q", "local/main"]);
        write(&repo.join("base.txt"), "two\n");
        git(&repo, &["commit", "-qam", "base moves"]);
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        git(&empty, &["init", "-q", "-b", "local/main"]);
        git(&empty, &["commit", "-q", "--allow-empty", "-m", "root"]);

        let lc = root.join("lc");
        let row = |id: &str, state: &str, extra: &str| {
            format!(
                r#"{{"bead_id":"{id}","state":"{state}","tip":"{}"{extra}}}"#,
                tips[id]
            )
        };
        let rows = [
            row("sp-capped", "SUBMITTED", ""),
            row("sp-clean", "SUBMITTED", ""),
            row("sp-conflict", "CERTIFIED", ""),
            row("sp-dupe", "SUBMITTED", ""),
            row("sp-ejected", "SUBMITTED", &format!(r#","ejected_red_tip":"{}""#, tips["sp-ejected"])),
            row("sp-gone", "SUBMITTED", ""),
            row("sp-held", "SUBMITTED", r#","holds":["operator"]"#),
            row("sp-ident", "SUBMITTED", ""),
            r#"{"bead_id":"sp-moved","state":"SUBMITTED","tip":"0000000000000000000000000000000000000000"}"#.to_string(),
            row("sp-nocontent", "SUBMITTED", ""),
            row("sp-onheld", "SUBMITTED", ""),
            row("sp-seen", "SUBMITTED", &format!(r#","sifted_tip":"{}""#, tips["sp-seen"])),
            row("sp-stacked", "SUBMITTED", ""),
            row("sp-target", "SUBMITTED", ""),
        ];
        write(&lc.join("rows.json"), &format!("[{}]", rows.join(",")));
        for id in tips.keys() {
            let (state, extra) = match id.as_str() {
                "sp-broken" => ("REWORK", String::new()),
                "sp-gone" => ("DROPPED", String::new()),
                "sp-conflict" => ("CERTIFIED", String::new()),
                "sp-onheld" => (
                    "SUBMITTED",
                    format!(r#","stack":"{{\"sp-held\":\"{}\"}}""#, tips["sp-held"]),
                ),
                _ => ("SUBMITTED", String::new()),
            };
            write(
                &lc.join(format!("show/{id}.json")),
                &format!(
                    r#"{{"bead":{{"state":"{state}","version":4,"tip":"{}"{extra}}}}}"#,
                    tips[id]
                ),
            );
            if id != "sp-nocontent" {
                let description = if id == "sp-target" {
                    "Target: p50 < 2s"
                } else {
                    ""
                };
                let deps = if id == "sp-clean" {
                    r#","dependencies":[{"depends_on_id":"sp-broken","type":"blocks"}]"#
                } else {
                    ""
                };
                write(
                    &lc.join(format!("content/{id}.json")),
                    &format!(
                        r#"{{"id":"{id}","title":"{id}","description":"{description}","notes":"","priority":2{deps}}}"#
                    ),
                );
            }
        }

        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("spira-lc"), LC_STUB);
        let home = spira_config::process::fixture_registry_home(root);
        testkit::write_exe(home.join("bead.sh"), BEAD_STUB);
        write(
            &root.join("queue/spira/open"),
            &format!("pr=7\nmembers=sp-inround:{}\n", tips["sp-inround"]),
        );
        write(
            &root.join("brain/wiki/projects/other/identity.md"),
            "## Identity terms\n- Zorblax\n",
        );
        let w = World {
            root: root.to_path_buf(),
            home,
            repo,
            tips,
        };
        w.reset();
        w
    }

    /// Forgets every verdict a run left, so the next run screens the same pool afresh.
    pub fn reset(&self) {
        let _ = fs::remove_dir_all(self.root.join("run"));
        write(&self.root.join("run/sift/send-backs/sp-capped"), "3\n");
        for f in ["effects.log", "inbox.log"] {
            let _ = fs::remove_file(self.root.join(f));
        }
    }

    pub fn env(&self) -> Env {
        Env {
            home: self.home.clone(),
            run: self.root.join("run"),
            queue_dir: self.root.join("queue"),
            lc_bin: self.root.join("bin/spira-lc").display().to_string(),
            lc_timeout: 30,
            wiki: self.root.join("brain").display().to_string(),
            identity_projects: "wiki/projects".into(),
            inbox: Ok(self.root.join("inbox.log").display().to_string()),
        }
    }

    fn rows(&self) -> [(&'static str, String, &'static str); 4] {
        [
            ("spira", self.repo.display().to_string(), "queue.local"),
            (
                "empty",
                self.root.join("empty").display().to_string(),
                "queue.local",
            ),
            (
                "broken",
                self.root.join("nowhere").display().to_string(),
                "queue.local",
            ),
            ("other", self.root.join("other").display().to_string(), "pr"),
        ]
    }

    pub fn registry(&self) -> Registry {
        let map: String = self
            .rows()
            .iter()
            .map(|(n, p, l)| format!("{n}|{p}|{l}|local/main||\n"))
            .collect();
        let env = [("SPIRA_HOME_REPO".to_string(), "spira".to_string())]
            .into_iter()
            .collect();
        Registry::new(Some(&map), &env, &self.home)
    }

    /// The process environment that runs either entry point on this world through config.
    pub fn process_env(&self) -> Vec<(String, String)> {
        let s = |p: PathBuf| p.display().to_string();
        let (run, queue, brain, inbox) = (
            s(self.root.join("run")),
            s(self.root.join("queue")),
            s(self.root.join("brain")),
            s(self.root.join("inbox.log")),
        );
        let toml = spira_config::process::fixture_toml(
            &self.root,
            &[
                ("SPIRA_RUN", &run),
                ("SPIRA_QUEUE_DIR", &queue),
                ("SPIRA_WIKI", &brain),
                ("SPIRA_IDENTITY_PROJECTS", "wiki/projects"),
                ("SPIRA_CONCIERGE_INBOX", &inbox),
            ],
        );
        let repo = self.repo.display().to_string();
        spira_config::set_paths_in_file(&toml, &[("repo.spira.path", repo.as_str())]).unwrap();
        let mut text = fs::read_to_string(&toml).unwrap();
        for (n, p, l) in self.rows().into_iter().filter(|(n, ..)| *n != "spira") {
            text.push_str(&format!(
                "\n[repo.{n}]\npath = \"{p}\"\nmode = \"{l}\"\nbase = \"local/main\"\n"
            ));
        }
        spira_config::validate(&text).unwrap();
        fs::write(&toml, text).unwrap();
        vec![
            ("SPIRA_TOML".into(), s(toml)),
            ("SPIRA_HOME".into(), s(self.home.clone())),
            ("SPIRA_NOW".into(), NOW.into()),
            (
                "PATH".into(),
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            ),
        ]
    }

    /// Every verdict a run left: the store writes and notes in order, the Concierge's inbox,
    /// and the screen's own records.
    pub fn verdicts(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in ["effects.log", "inbox.log"] {
            let text = fs::read_to_string(self.root.join(f)).unwrap_or_default();
            out.extend(text.lines().map(|l| format!("{f}: {l}")));
        }
        let mut files = Vec::new();
        collect(&self.root.join("run/sift"), &mut files);
        files.sort();
        for p in files {
            let rel = p.strip_prefix(&self.root).unwrap().display().to_string();
            out.extend(
                fs::read_to_string(&p)
                    .unwrap()
                    .lines()
                    .map(|l| format!("{rel}: {l}")),
            );
        }
        out
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

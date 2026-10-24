use filings_collect::{pass, parse_hosts, real::Real, State};
use std::path::Path;

fn filing(class: &str, action: &str, body: &str) -> String {
    format!(r#"{{"class":"{class}","action":"{action}","repo":"widgets","title":"vocab: {action}","body":"{body}","findings":[]}}"#)
}

struct Rig {
    dir: testkit::TempDir,
    real: Real,
}

impl Rig {
    fn new() -> Rig {
        let dir = testkit::TempDir::new("filings-collect-stub");
        let p = dir.path();
        std::fs::create_dir_all(p.join("home/filings/outbox")).unwrap();
        testkit::write_exe(p.join("ssh"), "#!/bin/bash\nfor a; do last=\"$a\"; done\nexec sh -c \"$last\"\n");
        let log = p.join("bead.log");
        testkit::write_exe(
            p.join("bead.sh"),
            &format!(
                "#!/bin/bash\nn=$(cat {c} 2>/dev/null || echo 0)\nverb=\"$1\"\nprev=\"\"\nfor a; do [ \"$prev\" = --body-file -o \"$prev\" = --append-body ] && body=\"$a\"; prev=\"$a\"; done\n{{ echo \"$verb $*\"; cat \"$body\"; echo ---; }} >> {l}\nif [ \"$verb\" = file ]; then echo $((n+1)) > {c}; echo sp-stub$((n+1)); fi\n",
                c = p.join("count").display(),
                l = log.display()
            ),
        );
        testkit::write_exe(p.join("lc"), "#!/bin/sh\necho '{\"bead\":{\"state\":\"OPEN\"}}'\n");
        let real = Real { ssh: p.join("ssh").to_string_lossy().into(), bead: p.join("bead.sh").to_string_lossy().into(), lc: p.join("lc").to_string_lossy().into() };
        Rig { dir, real }
    }

    fn outbox(&self) -> std::path::PathBuf {
        self.dir.path().join("home/filings/outbox")
    }

    fn put(&self, name: &str, text: &str) {
        std::fs::write(self.outbox().join(name), text).unwrap();
    }

    fn run(&self, state: &mut State) -> filings_collect::Outcome {
        let hosts = parse_hosts(&format!("prod=stub@host:{}", self.dir.path().join("home").display())).unwrap();
        pass(&self.real, &hosts, state, &mut |_| Ok(()))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(Path::new(self.dir.path()).join("bead.log")).unwrap_or_default()
    }

    fn outbox_len(&self) -> usize {
        std::fs::read_dir(self.outbox()).unwrap().count()
    }

    fn acked(&self) -> usize {
        std::fs::read_dir(self.dir.path().join("home/filings/acked")).map_or(0, |d| d.count())
    }
}

#[test]
fn one_outbox_filing_is_one_bead_and_is_acknowledged_then_a_second_pass_files_nothing() {
    let rig = Rig::new();
    let mut st = State::default();
    rig.put("c-1.json", &filing("mech", "open", "unmatched wording"));
    let o = rig.run(&mut st);
    assert_eq!((o.filed.len(), o.amended.len(), o.acked.len()), (1, 0, 1), "{o:?}");
    assert!(o.failed.is_empty(), "{o:?}");
    assert!(rig.log().contains("file vocab: open --for builder --repo widgets"), "{}", rig.log());
    assert!(rig.log().contains("## Build"), "{}", rig.log());
    assert_eq!((rig.outbox_len(), rig.acked()), (0, 1));
    let again = rig.run(&mut st);
    assert_eq!(again, filings_collect::Outcome::default());
    assert_eq!(rig.log().matches("\nfile ").count() + usize::from(rig.log().starts_with("file ")), 1);
}

#[test]
fn an_appended_filing_amends_the_same_bead() {
    let rig = Rig::new();
    let mut st = State::default();
    rig.put("c-1.json", &filing("mech", "open", "first"));
    rig.run(&mut st);
    rig.put("c-2.json", &filing("mech", "append", "second card"));
    let o = rig.run(&mut st);
    assert_eq!((o.filed.len(), o.amended.len(), o.acked.len()), (0, 1, 1), "{o:?}");
    assert_eq!(o.amended, vec!["sp-stub1".to_string()]);
    assert!(rig.log().contains("amend sp-stub1 --append-body"), "{}", rig.log());
    assert!(rig.log().contains("second card"), "{}", rig.log());
}

#[test]
fn an_unparseable_filing_alarms_naming_it_and_stays_in_the_outbox() {
    let rig = Rig::new();
    let mut st = State::default();
    rig.put("bad.json", "{not json");
    let o = rig.run(&mut st);
    assert!(o.failed.iter().any(|f| f.contains("prod/bad.json") && f.contains("cannot parse")), "{o:?}");
    assert_eq!((rig.outbox_len(), rig.acked(), rig.log()), (1, 0, String::new()));
}

#[test]
fn an_unreachable_host_alarms_naming_the_host() {
    let rig = Rig::new();
    let hosts = parse_hosts("prod=stub@host:/nonexistent/home").unwrap();
    let o = pass(&rig.real, &hosts, &mut State::default(), &mut |_| Ok(()));
    assert!(o.failed.iter().any(|f| f.starts_with("prod: cannot read the outbox")), "{o:?}");
}

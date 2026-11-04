//! No Spira→Spira child survives in this crate unless it is named (scheduler consolidation §2):
//! every site that builds a child of the store CLI, the unit tools, the claim/hel/strand
//! binaries, the metrics writer, mail or a shell carries a `FORK-EDGE` comment saying what it
//! still needs.

/// The program expressions a site may not run unmarked.
const FORBIDDEN: &[&str] = &[
    "\"spira-lc\"", "\"systemctl\"", "\"systemd-run\"", "\"slay\"", "\"slay.sh\"", "\"tsd-write\"", "\"mail\"", "\"bash\"",
    "\"spira-config\"", "\"spira-claim\"", "\"hel\"", "\"strand\"", "\"watchtower\"",
    "lc_bin", "claim_bin", "hel_bin", "strand_bin", "tsd_bin", "slay_bin", "cfg.systemctl", "cfg.call", "cfg.launch",
];

const OPENERS: &[&str] = &["Spec::args_owned(", "Spec::new(", "Command::new(", "bounded("];

/// How many lines above a site its `FORK-EDGE` comment may sit.
const REACH: usize = 6;

/// `file:line` of every unmarked child of a forbidden program in `text`.
pub fn unmarked(name: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        for op in OPENERS {
            let Some(at) = code.find(op) else { continue };
            // The program is the first argument: up to the first `,` or `)`, possibly on the
            // next line.
            let rest: String = std::iter::once(&code[at + op.len()..]).chain(lines.iter().skip(n + 1).take(2).copied()).collect::<Vec<_>>().join(" ");
            let prog = rest.split([',', ')']).next().unwrap_or("").trim();
            if !FORBIDDEN.iter().any(|f| prog.contains(f)) {
                continue;
            }
            let marked = lines[n.saturating_sub(REACH)..=n].iter().any(|l| l.contains("FORK-EDGE"));
            if !marked {
                found.push(format!("{name}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    found
}

#[test]
fn no_unmarked_spira_child_remains() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        // The fixtures re-express the doors as the command lines they replaced; this file
        // names the programs it looks for.
        if p.extension().is_none_or(|x| x != "rs") || name == "tests.rs" || name == "fork_edges.rs" {
            continue;
        }
        found.extend(unmarked(&name, &std::fs::read_to_string(&p).unwrap()));
    }
    assert!(found.is_empty(), "children of Spira programs with no FORK-EDGE note:\n{}", found.join("\n"));
}

#[test]
fn a_planted_child_is_caught_and_a_marked_one_is_not() {
    let planted = "fn f(&self) {\n    let o = self.h.run(\n        Spec::args_owned(\n            self.cfg.lc_bin.clone(),\n            vec![\"list\".into()],\n        ),\n    );\n}\n";
    assert_eq!(unmarked("x.rs", planted).len(), 1, "a multi-line spira-lc child is caught");
    assert_eq!(unmarked("x.rs", "let c = Command::new(\"systemctl\");").len(), 1);
    assert_eq!(unmarked("x.rs", "let c = spira_config::bounded::bounded(\"hel\");").len(), 1);
    assert!(unmarked("x.rs", "// FORK-EDGE: unserved verb close\nlet c = Command::new(\"spira-lc\");").is_empty());
    assert!(unmarked("x.rs", "let c = Command::new(\"git\");").is_empty(), "git stays a child");
}

//! The landing pass starts no Spira program as a child — the store CLI, the unit manager, the
//! slayer, the metrics writer, mail, a shell, or hel — unless the site is marked FORK-EDGE with
//! what it still needs.

use std::path::Path;

fn banned() -> Vec<&'static str> {
    let mut b = hel::fork_scan::BANNED.to_vec();
    b.push("hel");
    b
}

#[test]
fn no_landing_pass_source_starts_a_spira_program_as_a_child() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let hits = hel::fork_scan::scan(&dir, &banned(), &[]);
    assert!(hits.is_empty(), "Spira children left in landing-pass (in-process them, or mark FORK-EDGE):\n{}", hits.join("\n"));
}

#[test]
fn a_planted_mail_or_hel_child_is_caught() {
    let planted = "let mut c = command(\"mail\");\nlet c = spira_config::bounded::bounded(\"hel\");\n";
    assert_eq!(hel::fork_scan::sites("x.rs", planted, &banned()).len(), 2);
}

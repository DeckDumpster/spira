//! The unit port reaches the manager over D-Bus only: no file of it starts the manager's own
//! command-line clients as a child.

use std::path::Path;

// Spelled in pieces so this file does not name a launcher itself.
const CLIENTS: [&str; 2] = [concat!("system", "ctl"), concat!("systemd", "-run")];

/// Every line of `src` that names a client as a program: a quoted name or path ending in one.
fn children(src: &str) -> Vec<String> {
    src.lines()
        .enumerate()
        .filter(|(_, l)| CLIENTS.iter().any(|c| l.contains(&format!("\"{c}\"")) || l.contains(&format!("/{c}\""))))
        .map(|(i, l)| format!("{}: {}", i + 1, l.trim()))
        .collect()
}

#[test]
fn no_file_of_the_port_starts_a_manager_client() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = [root.join("src/units.rs"), root.join("src/sources.rs"), root.join("../unit-run/src/dbus.rs")];
    let mut found = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        assert!(text.len() > 1000, "{} read as a stub", f.display());
        found.extend(children(&text).into_iter().map(|h| format!("{}:{h}", f.display())));
    }
    assert!(found.is_empty(), "manager clients started as children:\n{}", found.join("\n"));
}

#[test]
fn a_planted_client_is_caught_and_prose_is_not() {
    let ctl = CLIENTS[0];
    let run = CLIENTS[1];
    assert_eq!(children(&format!("let _ = Command::new(\"{ctl}\").arg(\"show\");")).len(), 1);
    assert_eq!(children(&format!("let p = \"/usr/bin/{run}\";\nspawn(p);")).len(), 1);
    assert_eq!(children(&format!("let argv = [\"{run}\", \"--wait\"];")).len(), 1);
    assert!(children(&format!("//! replaces `{ctl} show` and `{run} --wait --pipe`")).is_empty());
}

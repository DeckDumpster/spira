//! Generates `SpiraSection` and the `KEY => field` mapping from the config key registry, and
//! keeps the append-only key history in step. A key is declared once, in `spira/conf.d`.

#[path = "codegen.rs"]
mod codegen;

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let conf_d = manifest.join("../spira/conf.d");
    let conf_toml_d = manifest.join("../spira/conf.toml.d");
    let history = manifest.join("schema/spira-key-history.txt");
    for p in [&conf_d, &conf_toml_d, &history, &manifest.join("codegen.rs"), &manifest.join("build.rs")] {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let mut names: Vec<String> = fs::read_dir(&conf_d)
        .unwrap_or_else(|e| panic!("{}: {e}", conf_d.display()))
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .collect();
    names.sort();
    let mut emb = String::from("static EMBEDDED_CONF_D: &[(&str, &str)] = &[\n");
    for n in &names {
        let abs = conf_d.canonicalize().unwrap().join(n);
        emb.push_str(&format!("    ({n:?}, include_str!({:?})),\n", abs.display().to_string()));
    }
    emb.push_str("];\n");
    fs::write(out.join("embedded_conf_d.rs"), emb).unwrap();

    let keys = codegen::load(&conf_d, &conf_toml_d).unwrap_or_else(|e| panic!("config key registry: {e}"));
    fs::write(out.join("spira_section.rs"), codegen::section_rs(&keys)).unwrap();
    fs::write(out.join("spira_convert.rs"), codegen::convert_rs(&keys)).unwrap();

    let existing = fs::read_to_string(&history).unwrap_or_default();
    let merged = codegen::history_merge(&existing, &keys);
    if merged != existing {
        // Read-only checkout: tests/validate.rs then reports the stale history by name.
        let _ = fs::write(&history, merged);
    }
}

use std::{env, fs, path::Path};

fn main() {
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/rules");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().into_string().unwrap();
            let stem = n.strip_suffix(".rs")?;
            (stem != "mod").then(|| stem.to_string())
        })
        .collect();
    names.sort();
    let out = env::var("OUT_DIR").unwrap();
    let mods: String = names
        .iter()
        .map(|n| format!("#[path = {:?}]\npub mod {n};\n", dir.join(format!("{n}.rs"))))
        .collect();
    fs::write(Path::new(&out).join("rule_mods.rs"), mods).unwrap();
    let calls: String = names.iter().map(|n| format!("    all.extend(rules::{n}::rules());\n")).collect();
    let body = format!("{{\n    let mut all: Vec<Box<dyn Rule>> = Vec::new();\n{calls}    all\n}}\n");
    fs::write(Path::new(&out).join("all_rules.rs"), body).unwrap();
}

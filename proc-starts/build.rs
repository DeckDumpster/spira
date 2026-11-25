use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=bpf/proc_starts.bpf.c");
    println!("cargo:rerun-if-env-changed=PATH");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("proc_starts.bpf.o");
    // batch-job: compiling the bpf object runs as long as clang takes
    let status = Command::new("clang").args(["-target", "bpf", "-O2", "-g", "-c", "bpf/proc_starts.bpf.c", "-o"]).arg(&out).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("clang failed to build bpf/proc_starts.bpf.c ({s})"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => panic!("clang not found on PATH: add llvmPackages.clang-unwrapped from flake.nix"),
        Err(e) => panic!("cannot run clang: {e}"),
    }
}

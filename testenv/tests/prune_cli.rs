//! testenv cleans up after itself, run against the REAL compiled `testenv` with a fake
//! `podman` first on PATH. The fake keeps the store's records as files (images, containers,
//! volumes), answers the listings testenv reads from them, removes whatever it is told to,
//! and logs every call — so a test sees exactly what testenv asked podman to remove, and the
//! fake never refuses on testenv's behalf.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const FAKE_PODMAN: &str = r#"#!/usr/bin/env bash
S="$FAKE_PODMAN_STATE"
printf '%s\n' "$*" >> "$S/calls"
field() { sed -n "s/^$2=//p" "$1"; }
images_json() {
    sep=
    printf '['
    for f in "$S"/images/*; do
        [ -f "$f" ] || continue
        names=$(field "$f" names)
        dangling=$(field "$f" dangling)
        case "$1" in
            dangling=true) [ "$dangling" = 1 ] || continue ;;
            reference=*) case ",$names" in *",${1#reference=}:"*) ;; *) continue ;; esac ;;
        esac
        if [ -n "$names" ]; then nj=$(printf '"%s",' ${names//,/ }); nj="[${nj%,}]"; else nj=null; fi
        printf '%s{"Id":"%s","Names":%s,"Size":%s,"Containers":%s,"Created":%s,"Dangling":%s}' \
            "$sep" "$(basename "$f")" "$nj" "$(field "$f" size)" "$(field "$f" containers)" \
            "$(field "$f" created)" "$([ "$dangling" = 1 ] && echo true || echo false)"
        sep=,
    done
    printf ']\n'
}
case "$1 $2" in
    "images --format") images_json "$5" ;;
    "ps -a")
        if [ "$3" = --filter ]; then
            for f in "$S"/containers/*; do
                [ -f "$f" ] || continue
                case " $(field "$f" volumes) " in *" ${4#volume=} "*) basename "$f" ;; esac
            done
        else
            sep=
            printf '['
            for f in "$S"/containers/*; do
                [ -f "$f" ] || continue
                printf '%s{"Id":"%s","ImageID":"%s"}' "$sep" "$(basename "$f")" "$(field "$f" image)"
                sep=,
            done
            printf ']\n'
        fi ;;
    "rmi "*) rm "$S/images/$2" || exit 1 ;;
    "volume ls")
        sep=
        printf '['
        for f in "$S"/volumes/*; do
            [ -f "$f" ] || continue
            printf '%s{"Name":"%s","Mountpoint":"%s","MountCount":%s}' \
                "$sep" "$(basename "$f")" "$(field "$f" mount)" "$(field "$f" count)"
            sep=,
        done
        printf ']\n' ;;
    "volume rm") rm -rf "$(field "$S/volumes/$3" mount)" && rm "$S/volumes/$3" || exit 1 ;;
    "container exists") exit 1 ;;
    "image exists") exit 0 ;;
    "info --format") echo true ;;
    "run "*) exit 1 ;;
esac
exit 0
"#;

struct Fixture {
    _t: testkit::TempDir,
    root: PathBuf,
    state: PathBuf,
    path: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let t = testkit::TempDir::new(name);
        let root = t.path().to_path_buf();
        let state = root.join("podman");
        for d in ["images", "containers", "volumes"] {
            fs::create_dir_all(state.join(d)).unwrap();
        }
        fs::write(state.join("calls"), "").unwrap();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("podman"), FAKE_PODMAN);
        let path = format!("{}:/usr/bin:/bin", bin.display());
        Fixture {
            _t: t,
            root,
            state,
            path,
        }
    }

    fn image(&self, id: &str, names: &[&str], created: i64, containers: u64) {
        let body = format!(
            "names={}\nsize={}\ncreated={created}\ncontainers={containers}\ndangling={}\n",
            names.join(","),
            3 << 20,
            u8::from(names.is_empty())
        );
        fs::write(self.state.join("images").join(id), body).unwrap();
    }

    fn container(&self, id: &str, image: &str, volumes: &[&str]) {
        let body = format!("image={image}\nvolumes={}\n", volumes.join(" "));
        fs::write(self.state.join("containers").join(id), body).unwrap();
    }

    /// A volume whose data is `bytes` of real (allocated) content, last written `age` ago.
    fn volume(&self, name: &str, bytes: usize, age: Duration, mounts: u64) -> PathBuf {
        let mount = self.root.join("volumes").join(name).join("_data");
        fs::create_dir_all(&mount).unwrap();
        let f = mount.join("artifact");
        fs::write(&f, vec![7u8; bytes]).unwrap();
        fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
        let body = format!("mount={}\ncount={mounts}\n", mount.display());
        fs::write(self.state.join("volumes").join(name), body).unwrap();
        mount
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.state.join("calls"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn testenv(&self, args: &[&str], declare: &[(&str, &str)]) -> (Output, String) {
        let toml = spira_config::process::fixture_toml(&self.root, declare);
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let out = Command::new(env!("CARGO_BIN_EXE_testenv"))
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", &self.path)
            .env("SPIRA_HOME", repo().join("spira"))
            .env("SPIRA_TOML", toml)
            .env("SPIRA_TESTENV_HARNESS", repo())
            .env("FAKE_PODMAN_STATE", &self.state)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out, text)
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn a_prune_keeps_the_current_image_its_predecessor_and_what_a_container_uses_and_removes_the_rest()
{
    let f = Fixture::new("prune-images");
    let cur = "localhost/spira-testenv:cur";
    f.image("cccc00000000cccc", &[cur], 100, 0);
    f.image("3333000000003333", &["localhost/spira-testenv:s3"], 90, 0);
    f.image("2222000000002222", &["localhost/spira-testenv:s2"], 80, 0);
    f.container("ctr-on-s2", "2222000000002222", &[]);
    f.image("1111000000001111", &["localhost/spira-testenv:s1"], 70, 0);
    f.image(
        "bbbb00000000bbbb",
        &["docker.io/library/ubuntu:24.04"],
        10,
        0,
    );
    let dangling: Vec<String> = (0..40).map(|i| format!("dddd{i:012}")).collect();
    for (i, id) in dangling.iter().enumerate() {
        f.image(id, &[], 50 + i as i64, 0);
    }
    f.image("eeee00000000eeee", &[], 40, 0);
    f.container("ctr-on-dangling", "eeee00000000eeee", &[]);

    let (out, text) = f.testenv(&["container", "prune", "--current", cur], &[]);
    assert!(out.status.success(), "prune exits 0: {text}");

    let survivors: Vec<String> = fs::read_dir(f.state.join("images"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for keep in [
        "cccc00000000cccc",
        "3333000000003333",
        "2222000000002222",
        "bbbb00000000bbbb",
        "eeee00000000eeee",
    ] {
        assert!(
            survivors.contains(&keep.to_string()),
            "{keep} survives: {text}"
        );
    }
    assert_eq!(
        survivors.len(),
        5,
        "everything else is removed: {survivors:?}\n{text}"
    );

    let calls = f.calls();
    let removed: Vec<&String> = calls.iter().filter(|c| c.starts_with("rmi ")).collect();
    assert_eq!(
        removed.len(),
        41,
        "one rmi per superseded or dangling image, by id: {calls:?}"
    );
    for id in dangling
        .iter()
        .map(String::as_str)
        .chain(["1111000000001111"])
    {
        assert!(
            calls.contains(&format!("rmi {id}")),
            "{id} removed by its id: {calls:?}"
        );
        let logged = text
            .lines()
            .any(|l| l.contains("prune: removed") && l.contains(&id[..12]) && l.contains("3.0M"));
        assert!(
            logged,
            "the removal of {id} is logged with its size: {text}"
        );
    }
}

#[test]
fn the_next_unit_run_sweeps_the_target_volumes_under_the_cap_and_leaves_a_mounted_one_alone() {
    let f = Fixture::new("sweep-volumes");
    let mib = 1 << 20;
    let unit = "spira-unit-cargo-target";
    let mounted = f.volume(unit, mib / 4, Duration::from_secs(9 * 86400), 1);
    f.container("ctr-unit", "cccc00000000cccc", &[unit]);
    let old = f.volume(
        &format!("{unit}-old"),
        2 * mib,
        Duration::from_secs(5 * 86400),
        0,
    );
    let recent = f.volume(&format!("{unit}-new"), mib / 4, Duration::from_secs(60), 0);
    let other = f.volume(
        "spira-warm-0-cargo-reg",
        4 * mib,
        Duration::from_secs(30 * 86400),
        0,
    );
    let name = format!("prune-cli-{}", std::process::id());
    let checkout = f.root.join("checkout");
    fs::create_dir_all(&checkout).unwrap();

    let (_, text) = f.testenv(
        &[
            "container",
            "up",
            "--no-build",
            "--name",
            &name,
            "--checkout",
            checkout.to_str().unwrap(),
            "--cargo-target-volume",
            unit,
        ],
        &[("SPIRA_TESTENV_TARGET_CAP_MIB", "1")],
    );
    let _ = fs::remove_file(format!("/tmp/{name}.owner"));

    let calls = f.calls();
    let removals: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("volume rm"))
        .collect();
    assert_eq!(
        removals,
        [&format!("volume rm {unit}-old")],
        "only the least recently used unmounted target volume goes: {calls:?}\n{text}"
    );
    assert!(!old.exists(), "its data is gone: {text}");
    assert!(
        mounted.join("artifact").is_file(),
        "the volume a container mounts is untouched: {text}"
    );
    assert!(
        recent.join("artifact").is_file(),
        "the sweep stops once under the cap: {text}"
    );
    assert!(
        other.join("artifact").is_file(),
        "a volume the unit tier does not use is never swept: {text}"
    );
    assert!(
        text.contains(&format!("sweep: removed volume {unit}-old (2.0M)")),
        "the removal is logged with its size: {text}"
    );
    let rm_at = calls
        .iter()
        .position(|c| c.starts_with("volume rm"))
        .unwrap();
    let run_at = calls
        .iter()
        .position(|c| c.starts_with("run "))
        .expect("up reaches its run");
    assert!(
        rm_at < run_at,
        "the sweep runs before the run mounts the volume: {calls:?}"
    );
}

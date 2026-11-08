use super::*;
use std::os::unix::fs::{symlink, MetadataExt};

const MANAGER: &str = "user.slice/user-1000.slice/user@1000.service";
const DWARF_UNIT: &str = "spira-run-sentinel_2dcall-builder_2d1762500000000_2d0.service";
const SCOPE: &str = "tmux-spawn-abc.scope";

const MOUNTINFO: &str = "\
25 1 252:0 / / rw,relatime shared:1 - ext4 /dev/mapper/vg-lv rw
30 25 8:17 / /srv/ssd-a rw,relatime shared:5 - ext4 /dev/sdb1 rw
31 25 8:33 / /srv/ssd-b rw,relatime shared:6 - ext4 /dev/sdc1 rw
32 25 8:33 /worktree /h/x/run/worktree rw,relatime shared:6 - ext4 /dev/sdc1 rw
33 25 8:17 /run /h/x/run rw,relatime shared:5 - ext4 /dev/sdb1 rw
34 25 0:27 / /proc rw - proc proc rw
";

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn environ(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={v}\0")).collect()
}

/// A fake `/sys`: sdb (8:16) with sdb1 (8:17), sdc (8:32) with sdc1 (8:33), and dm-0 (252:0).
fn plant_sys(sys: &Path) {
    for (disk, dmaj, part, pmaj) in [("sdb", "8:16", "sdb1", "8:17"), ("sdc", "8:32", "sdc1", "8:33")] {
        let d = sys.join("devices/pci0/block").join(disk);
        write(&d.join("dev"), &format!("{dmaj}\n"));
        write(&d.join(part).join("dev"), &format!("{pmaj}\n"));
        write(&d.join(part).join("partition"), "1\n");
        std::fs::create_dir_all(sys.join("dev/block")).unwrap();
        symlink(format!("../../devices/pci0/block/{disk}"), sys.join("dev/block").join(dmaj)).unwrap();
        symlink(format!("../../devices/pci0/block/{disk}/{part}"), sys.join("dev/block").join(pmaj)).unwrap();
    }
    let dm = sys.join("devices/virtual/block/dm-0");
    write(&dm.join("dev"), "252:0\n");
    symlink("../../devices/virtual/block/dm-0", sys.join("dev/block/252:0")).unwrap();
}

struct Fixture {
    _dir: testkit::TempDir,
    roots: Roots,
    worktree: PathBuf,
    build: PathBuf,
    hand: PathBuf,
}

fn fixture() -> Fixture {
    let dir = testkit::TempDir::new("runtime-usage");
    let root = dir.path().to_path_buf();
    let roots = Roots { cgroup: root.join("cg"), proc_dir: root.join("proc"), sys: root.join("sys"), run_dir: root.join("run") };
    plant_sys(&roots.sys);
    let app = roots.cgroup.join(MANAGER).join("app.slice");

    // The dwarf: its main process in the unit, a cargo child one cgroup down.
    let unit = app.join(DWARF_UNIT);
    write(&unit.join("cpu.stat"), "usage_usec 12500000\nuser_usec 10000000\nsystem_usec 2500000\nnr_periods 0\n");
    write(&unit.join("io.stat"), "8:32 rbytes=4096 wbytes=3000000000 rios=1 wios=9 dbytes=0 dios=0\n8:16 rbytes=8192 wbytes=1000 rios=2 wios=1 dbytes=0 dios=0\n7:0 rbytes=512 wbytes=0 rios=1 wios=0 dbytes=0 dios=0\n");
    write(&unit.join("memory.current"), "314572800\n");
    write(&unit.join("memory.peak"), "943718400\n");
    write(&unit.join("cgroup.procs"), "101\n");
    write(&unit.join("leaf/cgroup.procs"), "102\n");

    // A unit that is not a dwarf, and spira-runtime's own.
    write(&app.join("lc-serve.service/cgroup.procs"), "50\n");
    write(&app.join("spira-runtime-prod.service/cgroup.procs"), "60\n");

    // The Concierge's session scope, where a hand build's cargo runs.
    let scope = app.join(SCOPE);
    write(&scope.join("cpu.stat"), "usage_usec 7000000\n");
    write(&scope.join("io.stat"), "8:16 rbytes=0 wbytes=6700000000 rios=0 wios=99 dbytes=0 dios=0\n");
    write(&scope.join("memory.current"), "1000\n");
    write(&scope.join("memory.peak"), "2000\n");
    write(&scope.join("cgroup.procs"), "201\n202\n");

    let worktree = roots.run_dir.join("worktree/sp-abc");
    let build = root.join("warm/slot.1");
    let hand = roots.run_dir.join("worktree/sp-xyz-hand");
    write(&worktree.join("src/lib.rs"), &"x".repeat(10_000));
    // What the dwarf leaves on disk: its worktree's link into the checkout, and its name.
    write(&worktree.join(".git"), "gitdir: /h/x/spira/.git/worktrees/sp-abc\n");
    write(&roots.run_dir.join("dwarf-builder-sp-abc.name"), "fjalar");
    write(&roots.run_dir.join("dwarf-builder-sp-abd.name"), "other");
    write(&build.join("debug/libfoo.rlib"), &"y".repeat(20_000));
    write(&hand.join("Cargo.toml"), &"z".repeat(3_000));
    write(&hand.join("target/debug/bar"), &"w".repeat(40_000));

    let p = &roots.proc_dir;
    write(&p.join("self/cgroup"), &format!("0::/{MANAGER}/app.slice/spira-runtime-prod.service\n"));
    write(&p.join("self/mountinfo"), MOUNTINFO);
    // The dwarf process itself carries no identity; its child does, beside a secret.
    write(&p.join("101/environ"), &environ(&[("HOME", "/h/x"), ("LC_PASSWORD", "hunter2")]));
    write(&p.join("101/cgroup"), &format!("0::/{MANAGER}/app.slice/{DWARF_UNIT}\n"));
    write(
        &p.join("102/environ"),
        &environ(&[("LC_PASSWORD", "hunter2"), ("BEAD_ID", "sp-abc"), ("CARGO_TARGET_DIR", &build.display().to_string())]),
    );
    write(&p.join("102/cgroup"), &format!("0::/{MANAGER}/app.slice/{DWARF_UNIT}/leaf\n"));
    symlink(&worktree, p.join("102/cwd")).unwrap();
    symlink("/", p.join("101/cwd")).unwrap();
    // The hand build's cargo, in the session scope; its sibling there is the Concierge.
    write(&p.join("201/environ"), &environ(&[("CARGO_TARGET_DIR", &hand.join("target").display().to_string())]));
    write(&p.join("201/cgroup"), &format!("0::/{MANAGER}/app.slice/{SCOPE}\n"));
    symlink(&hand, p.join("201/cwd")).unwrap();
    write(&p.join("202/environ"), "");
    write(&p.join("202/cgroup"), &format!("0::/{MANAGER}/app.slice/{SCOPE}\n"));
    symlink("/h/x", p.join("202/cwd")).unwrap();
    // A process outside every cgroup the sampler reads.
    write(&p.join("50/cgroup"), &format!("0::/{MANAGER}/app.slice/lc-serve.service\n"));
    symlink("/", p.join("50/cwd")).unwrap();

    Fixture { _dir: dir, roots, worktree, build, hand }
}

fn allocated(paths: &[&Path]) -> u64 {
    fn walk(p: &Path) -> u64 {
        let m = std::fs::symlink_metadata(p).unwrap();
        let own = m.blocks() * 512;
        if m.is_dir() {
            own + std::fs::read_dir(p).unwrap().map(|e| walk(&e.unwrap().path())).sum::<u64>()
        } else {
            own
        }
    }
    paths.iter().map(|p| walk(p)).sum()
}

#[test]
fn cgroup_files_parse_to_their_counters() {
    assert_eq!(cpu_usage_usec("usage_usec 609769044\nuser_usec 414257398\nsystem_usec 195511646\n"), Some(609_769_044));
    assert_eq!(cpu_usage_usec("user_usec 1\n"), None, "no usage_usec is no reading, not zero");
    assert_eq!(
        io_stat("8:48 rbytes=202644996096 wbytes=1562428784640 rios=6920043 wios=8189743 dbytes=0 dios=0\n252:0 rbytes=7 wbytes=9 rios=1 wios=1 dbytes=0 dios=0\n\n"),
        vec![("8:48".to_string(), 202_644_996_096, 1_562_428_784_640), ("252:0".to_string(), 7, 9)]
    );
    assert_eq!(io_stat(""), vec![], "a cgroup that has done no IO has an empty io.stat");
    assert_eq!(scalar("2189565952\n"), Some(2_189_565_952));
    assert_eq!(scalar("max\n"), None);
}

#[test]
fn a_disk_is_named_by_the_mount_of_its_filesystem_root() {
    let f = fixture();
    assert_eq!(whole_disk(&f.roots.sys, "8:33"), Some(("8:32".into(), "sdc".into())), "a partition walks up to its disk");
    assert_eq!(whole_disk(&f.roots.sys, "8:32"), Some(("8:32".into(), "sdc".into())));
    assert_eq!(whole_disk(&f.roots.sys, "252:0"), Some(("252:0".into(), "dm-0".into())));
    assert_eq!(whole_disk(&f.roots.sys, "9:9"), None);
    let d = disks(MOUNTINFO, |mm| whole_disk(&f.roots.sys, mm));
    let got: Vec<(&str, &str, &str, &str)> = d.iter().map(|(k, d)| (k.as_str(), d.device.as_str(), d.mount.as_str(), d.role.as_str())).collect();
    assert_eq!(got, [("252:0", "dm-0", "/", "root"), ("8:16", "sdb", "/srv/ssd-a", "ssd-a"), ("8:32", "sdc", "/srv/ssd-b", "ssd-b")], "bind mounts of a subtree do not name the disk");
}

#[test]
fn only_the_identity_keys_leave_an_environ() {
    let e = env_of(environ(&[("LC_PASSWORD", "hunter2"), ("BEAD_ID", "sp-1"), ("CARGO_TARGET_DIR", "/t"), ("GH_TOKEN", "x"), ("NOEQUALS", "")]).as_bytes());
    assert_eq!(e, BTreeMap::from([("BEAD_ID".to_string(), "sp-1".to_string()), ("CARGO_TARGET_DIR".to_string(), "/t".to_string())]));
    assert!(env_of(b"").is_empty());
}

#[test]
fn a_directory_is_the_bytes_allocated_under_it() {
    let f = fixture();
    assert_eq!(dir_bytes(&f.worktree), allocated(&[&f.worktree]));
    assert!(dir_bytes(&f.worktree) >= 10_000, "{}", dir_bytes(&f.worktree));
    assert_eq!(dir_bytes(&f.roots.run_dir.join("no-such")), 0);
}

#[test]
fn each_live_dwarf_and_each_hand_scope_is_one_sample() {
    let f = fixture();
    let got = sample(&f.roots);
    assert_eq!(got.iter().map(|s| (s.kind, s.unit.as_str())).collect::<Vec<_>>(), [(Kind::Dwarf, DWARF_UNIT), (Kind::Hand, SCOPE)], "{got:#?}");

    let d = &got[0];
    assert_eq!((d.rune.as_str(), d.name.as_str(), d.bead.as_str(), d.repo.as_str()), ("builder", "fjalar", "sp-abc", "spira"));
    assert_eq!(d.cgroup, format!("{MANAGER}/app.slice/{DWARF_UNIT}"));
    assert_eq!(d.started_ms, Some(1_762_500_000_000));
    assert_eq!(d.cpu_usec, 12_500_000);
    assert_eq!(d.cpu_seconds(), 12.5);
    assert_eq!((d.memory_current, d.memory_peak), (314_572_800, 943_718_400));
    assert_eq!(
        d.io,
        [
            DeviceIo { device: "7:0".into(), mount: String::new(), role: "7:0".into(), read_bytes: 512, write_bytes: 0 },
            DeviceIo { device: "sdb".into(), mount: "/srv/ssd-a".into(), role: "ssd-a".into(), read_bytes: 8192, write_bytes: 1000 },
            DeviceIo { device: "sdc".into(), mount: "/srv/ssd-b".into(), role: "ssd-b".into(), read_bytes: 4096, write_bytes: 3_000_000_000 },
        ],
        "each disk by its kernel name and its mount; one sysfs cannot name keeps its MAJ:MIN"
    );
    assert_eq!((d.worktree.as_str(), d.worktree_bytes), (f.worktree.to_str().unwrap(), Some(allocated(&[&f.worktree]))));
    assert_eq!((d.build_dir.as_str(), d.build_dir_bytes), (f.build.to_str().unwrap(), Some(allocated(&[&f.build]))), "the build directory is the one the dwarf's children were given");

    let h = &got[1];
    assert_eq!((h.rune.as_str(), h.name.as_str(), h.bead.as_str()), ("hand", "hand", "sp-xyz"));
    assert_eq!((h.cpu_usec, h.memory_current, h.memory_peak), (7_000_000, 1000, 2000));
    assert_eq!(h.io, vec![DeviceIo { device: "sdb".into(), mount: "/srv/ssd-a".into(), role: "ssd-a".into(), read_bytes: 0, write_bytes: 6_700_000_000 }]);
    assert_eq!((h.worktree.as_str(), h.worktree_bytes), (f.hand.to_str().unwrap(), Some(allocated(&[&f.hand]))));
    assert_eq!(h.build_dir, f.hand.join("target").display().to_string());
    assert_eq!(h.started_ms, None);
}

#[test]
fn no_dwarf_and_no_hand_build_is_no_sample() {
    let f = fixture();
    std::fs::remove_dir_all(f.roots.cgroup.join(MANAGER).join("app.slice").join(DWARF_UNIT)).unwrap();
    std::fs::remove_file(f.roots.proc_dir.join("201/cwd")).unwrap();
    symlink("/", f.roots.proc_dir.join("201/cwd")).unwrap();
    assert_eq!(sample(&f.roots), vec![]);
}

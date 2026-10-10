use sim::actors::{install, parse_specs};
use sim::fit::{fit_files, Durations};
use sim::units::{parse_timer, SECOND};
use sim::{EventKind, Rng, Sim};
use std::path::Path;
use testkit::TempDir;
use std::process::Command;

const SPECS: &str = include_str!("../actors.toml");
const DURATIONS: &str = include_str!("../durations.toml");
const SERVICE: &str = "[Service]\nType=oneshot\nExecStart=/bin/true\n";

fn units(dir: &Path, every: &str) {
    for unit in parse_specs(SPECS).unwrap().iter().map(|a| a.timer.trim_end_matches(".timer").to_string()) {
        std::fs::write(
            dir.join(format!("{unit}.timer")),
            format!("[Timer]\nOnBootSec=10s\nOnUnitActiveSec={every}\nUnit={unit}.service\n"),
        )
        .unwrap();
        std::fs::write(dir.join(format!("{unit}.service")), SERVICE).unwrap();
    }
}

fn dues(dir: &Path, actor: &str) -> Vec<u64> {
    let mut sim = Sim::new(1);
    let d = Durations::parse(DURATIONS).unwrap();
    install(&mut sim, &parse_specs(SPECS).unwrap(), dir, &d, 0, 1000 * SECOND).unwrap();
    sim.run();
    sim.trace
        .iter()
        .filter(|t| t.kind == EventKind::Due(actor.into()))
        .map(|t| t.time)
        .collect()
}

#[test]
fn changing_a_timer_unit_changes_the_cadence() {
    let tmp = TempDir::new("cadence");
    let dir = tmp.path().to_path_buf();
    units(&dir, "90s");
    let slow = dues(&dir, "landing-pass");
    units(&dir, "30s");
    let fast = dues(&dir, "landing-pass");
    assert_eq!(slow.len(), 12);
    assert_eq!(fast.len(), 34);
}

#[test]
fn shipped_units_drive_the_shipped_actors() {
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd");
    let d = dues(&shipped, "landing-pass");
    assert!(d.len() > 8, "{d:?}");
}

#[test]
fn a_non_oneshot_service_is_refused() {
    let tmp = TempDir::new("simple");
    let dir = tmp.path().to_path_buf();
    units(&dir, "90s");
    std::fs::write(dir.join("spira-landing-pass.service"), "[Service]\nType=simple\n").unwrap();
    let mut sim = Sim::new(1);
    let d = Durations::parse(DURATIONS).unwrap();
    let err = install(&mut sim, &parse_specs(SPECS).unwrap(), &dir, &d, 0, SECOND).unwrap_err();
    assert!(err.contains("oneshot"), "{err}");
}

#[test]
fn calendar_and_randomized_delay() {
    let t = "[Timer]\nOnCalendar=*-*-* *:07,37:00\nRandomizedDelaySec=300\n";
    let s = parse_timer(t).unwrap();
    let times = s.fire_times(0, 3600 * SECOND, &mut Rng::new(9));
    assert_eq!(times.len(), 2);
    let bases = [7 * 60 * SECOND, 37 * 60 * SECOND];
    assert!(times.iter().zip(bases).all(|(t, b)| *t >= b && *t <= b + 300 * SECOND));
    assert_ne!(times, bases);

    // 1970-01-01 is a Thursday: a Mon-only calendar first fires on day 4.
    let mon = parse_timer("[Timer]\nOnCalendar=Mon *-*-* 06:15:00\n").unwrap();
    let t = mon.fire_times(0, 10 * 86_400 * SECOND, &mut Rng::new(1));
    assert_eq!(t[0], (4 * 86_400 + 6 * 3600 + 15 * 60) * SECOND);
}

#[test]
fn unsupported_or_dead_timers_are_refused() {
    assert!(parse_timer("[Timer]\nOnUnitActiveSec=5min\n").is_err());
    assert!(parse_timer("[Timer]\nAccuracySec=1s\n").is_err());
    assert!(parse_timer("[Timer]\nOnCalendar=daily\n").is_err());
}

#[test]
fn fit_refuses_missing_and_empty_logs() {
    let tmp = TempDir::new("fit");
    let dir = tmp.path().to_path_buf();
    let gate = dir.join("gate.log");
    let land = dir.join("land.tsv");
    assert!(fit_files(&gate, &land).is_err());
    std::fs::write(&gate, "").unwrap();
    std::fs::write(&land, "").unwrap();
    assert!(fit_files(&gate, &land).unwrap_err().contains("empty"));
    std::fs::write(&gate, "garbage\n").unwrap();
    std::fs::write(&land, "garbage\n").unwrap();
    assert!(fit_files(&gate, &land).is_err());
}

#[test]
fn fit_binary_refuses_and_accepts() {
    let tmp = TempDir::new("bin");
    let dir = tmp.path().to_path_buf();
    let (gate, land, out) = (dir.join("g"), dir.join("l"), dir.join("o.toml"));
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sim-fit"))
            .args(["--gate-log", gate.to_str().unwrap(), "--land-times", land.to_str().unwrap()])
            .args(["--out", out.to_str().unwrap()])
            .output()
            .unwrap()
    };
    assert_eq!(run().status.code(), Some(1));
    assert!(!out.exists());
    let g: Vec<String> = (1..=8).map(|i| format!("t r b waited=0s ran={}s rc=0 pass", 100 + i * 10)).collect();
    let g = g.join("\n");
    let l: Vec<String> =
        (1..=8).map(|i| format!("t\tsp-x\tmerge=0\tland-local={}\ttotal={}\tphases=a:{}", 10 + i, 100 + i, i)).collect();
    let l = l.join("\n");
    std::fs::write(&gate, g).unwrap();
    std::fs::write(&land, l).unwrap();
    assert!(run().status.success());
    let d = Durations::parse(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(d.series["gate.ran"].n, 8);
    assert_eq!(d.series["gate.ran"].min, 110.0);
}

#[test]
fn sampled_durations_are_seeded_and_bounded() {
    let d = Durations::parse(DURATIONS).unwrap();
    let s = &d.series["gate.ran"];
    let draw = |seed| (0..50).map(|_| ()).scan(Rng::new(seed), |r, _| Some(s.sample_ms(r))).collect::<Vec<_>>();
    assert_eq!(draw(5), draw(5));
    assert_ne!(draw(5), draw(6));
    assert!(draw(5).iter().all(|ms| *ms as f64 >= s.min * 1000.0 - 1.0 && *ms as f64 <= s.max * 1000.0 + 1.0));
}

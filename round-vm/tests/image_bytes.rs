//! A round image's snapshot copies every block setup wrote, so setup removes what it
//! superseded, reports what is left, and trims before the image is taken.

#[macro_use]
extern crate spira_test_id;
const TEMPLATE: &str = include_str!("../src/template.sh");
const AMI_SETUP: &str = include_str!("../ami-setup.sh");
const GUEST: &str = include_str!("../../spira/round-image-preflight-guest.sh");
const SSM: &str = include_str!("../ssm-commands.jq");

fn code(script: &str) -> Vec<&str> {
    script.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()
}

fn line_with(lines: &[&str], needle: &str) -> usize {
    lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no code line has {needle:?}"))
}

#[test_id(tmsitgv)]
#[test]
fn the_template_prunes_what_the_testenv_build_left_untagged() {
    let t = code(TEMPLATE);
    let build = line_with(&t, "testenv -- container image");
    let prune = line_with(&t, "podman image prune -f >&2");
    let cache = line_with(&t, "podman image prune -f --build-cache >&2");
    let fetch = line_with(&t, "cargo fetch");
    assert!(build < prune && prune < fetch, "untagged images are pruned between the testenv build and cargo fetch");
    assert!(build < cache && cache < fetch, "the build cache is pruned between the testenv build and cargo fetch");
}

#[test_id(t1dubr5)]
#[test]
fn the_image_setup_ends_by_reporting_its_bytes_then_trimming_the_root() {
    let s = code(AMI_SETUP);
    let used = line_with(&s, "image-bytes used / %s");
    assert!(s[used].contains("df -B1 --output=used /"), "{}", s[used]);
    let consumers = line_with(&s, r#"for d in /nix /var/lib/containers "$cache_home" /root/.rustup /tmp; do"#);
    let du = line_with(&s, r#"[ ! -e "$d" ] || du -sxB1 "$d" | sed 's/^/image-bytes /'"#);
    let trim = line_with(&s, "fstrim -v /");
    assert!(used < consumers && consumers < du && du < trim, "the report is taken before the trim");
    assert_eq!(s[trim - 1], "sync");
    assert_eq!(trim + 2, s.len(), "only the trim's report follows it: {:?}", &s[trim..]);
    assert_eq!(s[trim + 1], r#"echo "image-bytes $trimmed""#);
}

#[test_id(tc9ibzs)]
#[test]
fn the_preflight_guest_replaces_the_trim_and_the_nested_prune() {
    let g = code(GUEST);
    line_with(&g, r"printf '#!/bin/sh\nexit 0\n' > /usr/local/bin/fstrim");
    assert!(g[line_with(&g, "chmod +x /usr/local/bin/systemctl")].contains("/usr/local/bin/fstrim"));
    let podman = line_with(&g, "cat > /usr/local/bin/podman <<'SHIM'");
    let end = podman + g[podman..].iter().position(|l| *l == "SHIM").expect("the podman shim ends");
    line_with(&g[podman..end], r#""image prune") exit 0 ;;"#);
}

#[test_id(tfehv56)]
#[test]
fn a_successful_setup_prints_its_byte_report_to_the_ssm_output() {
    let program = code(SSM).join("\n");
    assert!(program.contains(r#"else grep '^image-bytes ' \"$L\"; fi; exit $rc"#), "no success branch prints the image-bytes lines:\n{program}");
}

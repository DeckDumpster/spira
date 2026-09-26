#!/usr/bin/env python3
# render.py — substitute @KEY@ placeholders in a Spira systemd unit template.
#
# Shared by install.sh and unit-ensure.sh so both callers render identically.
# Before this, each carried its own copy of this logic; unit-ensure.sh's copy
# never learned SPIRA_SUPERVISE_BIN, SPIRA_SNAP_STALE_S or SPIRA_LANDING_PASS_BIN
# (added to install.sh's copy later), so it silently failed on any unit using
# them — an unresolved-placeholder error on every run, never noticed because
# the two copies were never compared. Naming every value instead of positional
# argv also stops that shape of drift: a caller that forgets a flag gets that
# flag's default, not the wrong value shifted into the next slot.
import argparse
import os
import re
import sys


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("template")
    p.add_argument("--home", default="")
    p.add_argument("--repo", default="")
    p.add_argument("--run", default="")
    p.add_argument("--db", default="")
    p.add_argument("--cockpit", default="")
    p.add_argument("--dolt-data", default="")
    p.add_argument("--testdb-data", default="")
    p.add_argument("--dolt", default="")
    p.add_argument("--prod", default="")
    p.add_argument("--instance", default="")
    p.add_argument("--testdb-port", default="")
    p.add_argument("--supervise-bin", default="")
    p.add_argument("--snap-stale-s", default="")
    p.add_argument("--landing-pass-bin", default="")
    p.add_argument("--watcher-name", default="")
    args = p.parse_args()

    m = {
        "SPIRA_HOME": args.home,
        "SPIRA_REPO": args.repo,
        "SPIRA_RUN": args.run,
        "SPIRA_DB": args.db,
        "SPIRA_COCKPIT": args.cockpit,
        "SPIRA_DOLT_DATA": args.dolt_data,
        "SPIRA_TESTDB_DATA": args.testdb_data,
        "DOLT": args.dolt,
        "SPIRA_PROD": args.prod,
        "SPIRA_INSTANCE": args.instance,
        "SPIRA_TESTDB_PORT": args.testdb_port,
        "SPIRA_SUPERVISE_BIN": args.supervise_bin,
        "SPIRA_SNAP_STALE_S": args.snap_stale_s,
        "SPIRA_LANDING_PASS_BIN": args.landing_pass_bin,
    }
    # FALLBACK: an empty SPIRA_PROD is the documented signal that no checkout split
    # is wanted — everything runs from the development checkout (SPIRA_HOME). An
    # empty string substituted into @SPIRA_PROD@ yields ExecStart=/sentinel.sh,
    # which is both wrong and silent (no unresolved placeholder remains).
    if not m["SPIRA_PROD"]:
        m["SPIRA_PROD"] = m["SPIRA_HOME"]
    m["SPIRA_PROD_COCK"] = os.path.dirname(m["SPIRA_PROD"]) + "/cockpit"
    m["SPIRA_PROD_ROOT"] = os.path.dirname(m["SPIRA_PROD"])

    text = open(args.template).read()
    if not m["DOLT"] and "@DOLT@" in text:
        sys.stderr.write(
            "render: %s: dolt is not on PATH; install dolt before rendering units that need it\n"
            % os.path.basename(args.template)
        )
        return 1
    out = re.sub(r"@([A-Z_]+)@", lambda x: m.get(x.group(1), x.group(0)), text)
    # Substitute %i with the watcher name for templates that use systemd's instance
    # specifier. Under per-instance naming there is no systemd @-template; %i is
    # only a placeholder that render replaces at install time.
    if args.watcher_name:
        out = out.replace("%i", args.watcher_name)
    # For spira-*.timer templates: rewrite Unit=spira-<svc>.service to the
    # instance-suffixed name. inst_name renames the timer FILE by appending
    # SPIRA_INSTANCE, but the explicit Unit= line in the template names the service
    # without that suffix — which defeats the file rename and points the installed
    # timer at the legacy plain-named service instead.
    tname = os.path.basename(args.template)
    if tname.startswith("spira-") and tname.endswith(".timer"):
        out = re.sub(
            r"^(Unit=spira-[A-Za-z0-9_-]+)\.service$",
            r"\g<1>-" + m["SPIRA_INSTANCE"] + ".service",
            out,
            flags=re.MULTILINE,
        )
    left = sorted(set(re.findall(r"@([A-Z_]+)@", out)))
    if left:
        sys.stderr.write(
            "render: %s has placeholders nothing fills: %s\n"
            % (tname, ", ".join(left))
        )
        return 1
    sys.stdout.write(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())

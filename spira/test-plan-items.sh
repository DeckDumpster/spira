#!/usr/bin/env bash
#
# test-plan-items.sh — the planning reader against beads' real column names: `plan items`,
# `plan edges` and `pane-facets` read the content store over spira-lc's connection, and the
# claim decision and the panes depend on what they return.
#
# The content store's database is the one the bead store's metadata names, never a default;
# a store that names none is refused by name. The fixture's tables carry beads' real column
# names (depends_on_issue_id, not depends_on_id): a reader written against an invented column
# passes every suite that reads content through a shim.
#
# host-reason: starts its own disposable `dolt sql-server` (the content store is a dolt
# database); testenv-batch.sh already provides the container this suite executes in.
#
# defect: sp-9j0937
# tier: T2
# covers: spira-lc/src/plan.rs spira-lc/src/main.rs spira/testlib/lc-dolt-fixture.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/lc-dolt-fixture.sh"
TMP="$(mktemp -d)"
trap 'lcdolt_down; rm -rf "$TMP"' EXIT INT TERM
BIN="$(command -v spira-lc 2>/dev/null)"; [ -n "$BIN" ] || bail "spira-lc is not on PATH"
lcdolt_up || bail "the dolt server did not come up"
root_sql() { lcdolt_sql "$@"; }

# ── the content store: spira_lc reads it, and cannot write it ─────────────────────────
root_sql -q "CREATE DATABASE IF NOT EXISTS spira; CREATE TABLE IF NOT EXISTS spira.issues (id VARCHAR(64) PRIMARY KEY, title TEXT, priority INT, issue_type VARCHAR(32), status VARCHAR(32), defer_until DATETIME, created_at DATETIME, updated_at DATETIME, closed_at DATETIME, description TEXT, notes TEXT, external_ref VARCHAR(255)); CREATE TABLE IF NOT EXISTS spira.labels (issue_id VARCHAR(64), label VARCHAR(64), PRIMARY KEY (issue_id, label)); CREATE TABLE IF NOT EXISTS spira.dependencies (id CHAR(36) PRIMARY KEY, issue_id VARCHAR(255) NOT NULL, type VARCHAR(32) NOT NULL DEFAULT 'blocks', depends_on_issue_id VARCHAR(255), depends_on_wisp_id VARCHAR(255), depends_on_external VARCHAR(255))"

# ── plan items reads beads' real column names (sp-9j0937) ──────────────────────────
# The claim decision's content read once named spira.dependencies.depends_on_id, a column no
# beads schema has (the real one is depends_on_issue_id), and every prod claim check failed
# while every suite passed: this fixture invented the column and the suites read content
# through a bd shim, never this query. The fixture above now carries beads' real column names,
# and this runs the query itself against them.
# The content store's database is the one the bead store's metadata names (sp-jf7spl), never
# a default: this fixture's store declares `spira`, the database built above.
mkdir -p "$TMP/store/.beads" "$TMP/store-unnamed/.beads"
printf '{"dolt_mode":"server","dolt_database":"spira"}\n' > "$TMP/store/.beads/metadata.json"
printf '{"dolt_mode":"server"}\n' > "$TMP/store-unnamed/.beads/metadata.json"
tl_config SPIRA_DB="$TMP/store"
# A store whose metadata names no database is refused by name (law-a-config-value-has-no-fallback).
out="$(SPIRA_TOML="$(tl_layer SPIRA_DB="$TMP/store-unnamed")" "$BIN" plan items --ids sp-row-a 2>&1)"; rc=$?
is "plan items refuses a store whose metadata names no database (exit 2)" "2" "$rc"
want "the refusal names the missing dolt_database" "names no dolt_database" "$out"
want "the refusal names the metadata file" "$TMP/store-unnamed/.beads/metadata.json" "$out"
out="$(SPIRA_TOML="$(tl_layer SPIRA_DB="$TMP/store-unnamed")" "$BIN" pane-facets repo-labels --valid known 2>&1)"; rc=$?
is "pane-facets refuses a store whose metadata names no database (exit 2)" "2" "$rc"
root_sql -q "INSERT INTO spira.issues (id, title, priority, issue_type, status, defer_until, created_at, updated_at, closed_at, description, notes, external_ref) VALUES ('sp-row-a','a',1,'task','open',NULL,NOW(),NOW(),NULL,'the body','the notes','basefail:rr:t.sh'),('sp-row-b','b',1,'task','open',NULL,NOW(),NOW(),NULL,NULL,NULL,NULL),('sp-row-bug','c',0,'bug','open',NULL,NOW(),NOW(),NULL,NULL,NULL,NULL),('sp-row-ev','e',2,'event','open',NULL,NOW(),NOW(),NULL,NULL,NULL,NULL),('sp-row-old','o',2,'task','closed',NULL,NOW() - INTERVAL 3 DAY,NOW(),NOW() - INTERVAL 2 DAY,NULL,NULL,NULL); INSERT INTO spira.labels (issue_id, label) VALUES ('sp-row-a','repo:known'),('sp-row-b','repo:stray'),('sp-row-bug','incident'); INSERT INTO spira.dependencies (id, issue_id, type, depends_on_issue_id) VALUES ('d1','sp-row-a','blocks','sp-row-b'); INSERT INTO spira.dependencies (id, issue_id, type, depends_on_external) VALUES ('d2','sp-row-a','blocks','ext:x')" >/dev/null 2>&1
out="$("$BIN" plan items --ids sp-row-a 2>&1)"
want "plan items reads depends_on_issue_id (beads' real column)" '"depends_on_id":"sp-row-b"' "$out"
out="$("$BIN" plan items --ids sp-row-bug 2>&1)"
nowant "plan items carries only the edges bd holds: sp-row-bug has none" '"depends_on_id"' "$out"
out_full="$("$BIN" plan items --full --ids sp-row-a 2>&1)"
want "plan items --full carries the description" '"description":"the body"' "$out_full"
want "plan items --full carries the notes" '"notes":"the notes"' "$out_full"
case "$out" in *"the body"*) want "plan items without --full omits the description" "absent" "present: $out" ;; *) want "plan items without --full omits the description" "absent" "absent" ;; esac
out="$("$BIN" plan items --external-contains basefail:rr: 2>&1)"
want "plan items --external-contains selects by external_ref" '"id":"sp-row-a"' "$out"
case "$out" in *'"id":"sp-row-b"'*) want "plan items --external-contains selects only matching rows" "absent" "present: $out" ;; *) want "plan items --external-contains selects only matching rows" "absent" "absent" ;; esac
out="$("$BIN" plan items --ids sp-row-a 2>&1)"
case "$out" in *ext:x*) want "plan items ignores non-issue edges (depends_on_external)" "absent" "present: $out" ;; *) want "plan items ignores non-issue edges (depends_on_external)" "absent" "absent" ;; esac

# ── pane-facets answers aggregates, never the table ───────────────────────────────────
out="$("$BIN" pane-facets inflow --window-secs 3600 2>&1)"
want "pane-facets inflow counts the window's work beads, not events or old closures" '"n":3' "$out"
want "pane-facets inflow counts a bug or incident as a defect" '"defect":1' "$out"
want "pane-facets inflow groups by kind" '["task",2]' "$out"
case "$out" in *sp-row-old*|*sp-row-ev*) want "pane-facets inflow rows omit events and old closures" "absent" "present: $out" ;; *) want "pane-facets inflow rows omit events and old closures" "absent" "absent" ;; esac
out="$("$BIN" pane-facets repo-labels --valid known 2>&1)"
want "pane-facets repo-labels counts a repo outside the valid set as unmapped" '"unmapped":1' "$out"
want "pane-facets repo-labels counts a bead with no repo label as absent" '"absent":2' "$out"


tl_summary

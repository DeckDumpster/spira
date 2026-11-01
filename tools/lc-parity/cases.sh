# cases.sh — sourced by capture.sh. `seed` builds the testdb through spira-lc's own verbs, so
# any backend can replay it from the recorded `_seed/` steps; each `case_<name>` is one golden,
# run on its own copy of the seeded store. `lc ARGS` records a call; `lc_in TEXT ARGS` gives it
# TEXT on stdin; `now EPOCH` moves the pinned clock for the calls after it.
# shellcheck shell=bash

T1=1111111111111111111111111111111111111111
T2=2222222222222222222222222222222222222222
T3=3333333333333333333333333333333333333333
T4=4444444444444444444444444444444444444444
T5=5555555555555555555555555555555555555555
T6=6666666666666666666666666666666666666666
LEASE=1767229200

claim() { lc event bead "$1" --expect READY --version 0 --actor test --kind "{\"Claim\":{\"holder\":\"dwarf-1\",\"lease_until\":$LEASE}}"; }
submit() { claim "$1"; lc event bead "$1" --expect WORKING --version 1 --actor test --kind "{\"Submit\":{\"tip\":\"$2\"}}"; }
certify() { submit "$1" "$2"; lc event bead "$1" --expect SUBMITTED --version 2 --actor test --kind "{\"GatePass\":{\"tip\":\"$2\",\"gate_key\":\"k1\"}}"; }

seed() {
    lc create-bead sp-ready --title "a ready bead" --priority 2 --repo spira
    lc create-bead sp-blocked --title "waits on sp-working" --priority 1 --repo spira
    lc create-bead sp-working --title "claimed" --repo spira
    claim sp-working
    lc dep-add sp-blocked sp-working
    lc create-bead sp-submitted --repo spira
    submit sp-submitted "$T1"
    lc create-bead sp-certified --repo spira
    certify sp-certified "$T2"
    lc create-bead sp-certified2 --repo other
    certify sp-certified2 "$T3"
    lc create-bead sp-rework --repo spira
    submit sp-rework "$T4"
    lc event bead sp-rework --expect SUBMITTED --version 2 --actor test --kind "{\"GateRed\":{\"tip\":\"$T4\",\"reason\":\"suites-failed\"}}"
    lc create-bead sp-held --repo spira
    lc hold sp-held manual "operator hold" test
    lc create-bead sp-asked --repo spira
    lc hold sp-asked ask "which way?" test
    lc create-ask sp-ask1 --work-bead sp-asked
    lc create-bead sp-delivering --repo spira
    certify sp-delivering "$T5"
    lc cut b-open --repo spira --head H1 --base B1 --members "sp-delivering:$T5" --actor test
    lc create-bead sp-landed --repo spira
    certify sp-landed "$T6"
    lc cut b-landed --repo spira --head H2 --base B2 --members "sp-landed:$T6" --actor test
    lc event batch b-landed --expect OPEN --version 1 --actor test --kind '{"CiStarted":{"run":"r1"}}'
    lc event batch b-landed --expect CI_RUNNING --version 2 --actor test --kind '"Green"'
    lc land b-landed --expect GREEN --version 3 --actor test --sha M1
    lc fact sp-working --kind claimed --actor test --cause seeded
}

# Reads.
case_show_ready() { lc show sp-ready; }
case_show_blocked() { lc show sp-blocked; }
case_show_delivering() { lc show sp-delivering; }
case_show_landed() { lc show sp-landed; }
case_show_absent() { lc show sp-nope; }
case_list() { lc list; }
case_list_live() { lc list --live; }
case_list_state() { lc list --state READY,REWORK; }
case_list_ids() { lc list --ids sp-ready,sp-held,sp-nope; }
case_list_repo() { lc list --repo other; }
case_list_hold() { lc list --hold manual; }
case_list_delivery() { lc list --delivery; }
case_list_batches() { lc list --batches; }
case_history_bead() { lc history sp-certified; }
case_history_batch() { lc history b-landed --machine batch; }
case_history_delivery() { lc history sp-delivering --machine delivery; }
case_event_seq() { lc event-seq; lc event-seq --since 10; }
case_event_continuity() { lc event-continuity; }
case_show_batch() { lc show-batch b-open; lc show-batch b-landed; lc show-batch b-nope; }
case_batches_of() { lc batches-of sp-delivering; lc batches-of sp-ready; }
case_state() { lc state sp-working; lc state sp-nope; }
case_holds() { lc holds sp-held; lc holds sp-ready; }
case_held() { lc held sp-held manual; lc held sp-held ask; }
case_list_held() { lc list-held manual; lc list-held ask; }
case_list_state_verb() { lc list-state READY; }
case_list_all() { lc list-all; }
case_show_ask() { lc show-ask sp-ask1; lc show-ask sp-asknope; }
case_list_asks() { lc list-asks; lc list-asks --state OPEN; }
case_facts() { lc facts --ids sp-working; lc facts --kinds claimed; }
case_ops_bead() { lc ops-bead sp-blocked; }
case_ops_graph() { lc ops-graph; }
case_ops_snapshot() { lc ops-snapshot; }
case_ops_gantt() { lc ops-gantt; }
case_ops_view() { lc ops-view ops_live; lc ops-view ops_round; lc ops-view ops_edges; }
case_ops_refusals() { lc ops-refusals 86400; }
case_mending_list() { lc mending list; }
case_usage() { lc no-such-verb; }

# The bead machine.
case_claim() { claim sp-ready; lc show sp-ready; }
case_claim_stale_version() { lc event bead sp-ready --expect READY --version 7 --actor test --kind "{\"Claim\":{\"holder\":\"dwarf-2\",\"lease_until\":$LEASE}}"; }
case_claim_wrong_state() { lc event bead sp-working --expect READY --version 1 --actor test --kind "{\"Claim\":{\"holder\":\"dwarf-2\",\"lease_until\":$LEASE}}"; }
case_claim_terminal() { lc event bead sp-landed --expect LANDED --version 5 --actor test --kind "{\"Claim\":{\"holder\":\"dwarf-2\",\"lease_until\":$LEASE}}"; }
case_submit() { lc event bead sp-working --expect WORKING --version 1 --actor test --kind "{\"Submit\":{\"tip\":\"$T1\"}}"; lc state sp-working; }
case_release() { lc release sp-working test; lc show sp-working; }
case_renew() { lc renew sp-working dwarf-1 1767232800; lc show sp-working; }
case_renew_wrong_holder() { lc renew sp-working dwarf-9 1767232800; }
case_holder_dead() { lc holder-dead sp-working test; lc state sp-working; }
case_phase() { lc phase sp-working dwarf-1 building; lc show sp-working; }
case_disposition() { lc disposition sp-working lapsed "a note" test; lc show sp-working; }
case_hold_unhold() { lc hold sp-ready wait "snooze-until:1767300000 later" test; lc holds sp-ready; lc unhold sp-ready wait test; lc holds sp-ready; }
case_unhold_absent() { lc unhold sp-ready manual test; }
case_reply() { lc reply sp-asked msg-1 test; lc holds sp-asked; }
case_withdraw_ask() { lc withdraw-ask sp-asked test; lc holds sp-asked; }
case_express() { lc express sp-ready test; lc list --express; lc unexpress sp-ready test; lc show sp-ready; }
case_certify_pass() { lc certify sp-submitted "$T1" pass "" test; lc show sp-submitted; }
case_certify_red() { lc certify sp-submitted "$T1" red suites-failed test; lc show sp-submitted; }
case_certify_infra() { lc certify sp-submitted "$T1" infra "" test; lc show sp-submitted; }
case_certify_wrong_tip() { lc certify sp-submitted "$T2" pass "" test; }
case_resubmit() { lc resubmit sp-rework "$T6" test; lc show sp-rework; }
case_resubmit_submitted() { lc resubmit-submitted sp-submitted "$T6" test; lc show sp-submitted; }
case_retip() { lc retip sp-rework "$T6" test "rebased" "checks ran" "$T6"; lc show sp-rework; }
case_retip_wrong_state() { lc retip sp-certified "$T6" test "rebased" "checks ran" "$T6"; }
case_absolve() { lc absolve sp-rework "$T4" test "flake" "reran green" "$T4"; lc show sp-rework; }
case_returned() { lc returned sp-delivering push-rejected test; lc show sp-delivering; }
case_supersede() { lc supersede sp-ready sp-blocked test; lc show sp-ready; }
case_content_on_base() { lc content-on-base sp-certified "proof-1" test; lc show sp-certified; }
case_deliver_push() { lc deliver push-delivered sp-delivering M9; lc show sp-delivering; }
case_deliver_usage() { lc deliver bogus; }

# Rows written outside the event log.
case_create_bead() { lc create-bead sp-new --title "new" --priority 3 --repo spira; lc show sp-new; lc create-bead sp-new --title "again"; lc list --ids sp-new; }
case_create_bead_submitted() { lc create-bead sp-born --submitted-tip "$T1"; lc show sp-born; }
case_create_bead_bad_id() { lc create-bead "not an id"; }
case_delete_bead_row() { lc delete-bead-row sp-ready; lc show sp-ready; }
case_set_repo() { lc set-repo sp-ready other; lc list --ids sp-ready; lc set-repo sp-ready --clear; lc list --ids sp-ready; }
case_set_repo_only_if() { lc set-repo sp-ready elsewhere --only-if nope; lc list --ids sp-ready; }
case_dep_add_remove() { lc dep-add sp-ready sp-held; lc show sp-ready; lc dep-remove sp-ready sp-held; lc show sp-ready; }
case_claim_verdicts() {
    lc_in '{"computed_at":"2026-01-01T00:00:00Z","entries":[{"id":"sp-ready","verdict":"claimable","runes":["builder"]},{"id":"sp-blocked","verdict":"blocked","blocker":"sp-working"}]}' claim-verdicts
    lc list --ids sp-ready,sp-blocked
}
case_claim_verdicts_malformed() { lc_in '{"entries":7}' claim-verdicts; }
case_batch_progress() { lc_in '{"pass":1,"suites_done":3}' batch-progress b-open; lc show-batch b-open; }
case_fact() { lc fact sp-ready --kind claimed --actor test --cause case; lc facts --ids sp-ready; }

# The ask machine.
case_create_ask() { lc create-ask sp-ask2 --work-bead sp-held; lc show-ask sp-ask2; }
case_close_ask() { lc close-ask sp-ask1 --exit answered --quote "this way" --actor test --channel chat; lc show-ask sp-ask1; }

# Batches and their cascades.
case_cut() { lc cut b-new --repo spira --head H3 --base B3 --members "sp-certified:$T2,sp-submitted:$T1" --actor test; lc show-batch b-new; lc show sp-certified; }
case_cut_rework_refused() { lc cut b-new --repo spira --head H3 --base B3 --members "sp-rework:$T4" --actor test; lc show sp-rework; }
case_stack() { lc stack b-open --members "sp-certified:$T2" --actor test; lc batches-of sp-certified; }
case_stage_promote() {
    lc stage b-next --repo spira --head H4 --base B4 --members "sp-certified2:$T3" --actor test --parent b-open
    lc show-batch b-next
    lc promote b-next --head H5 --base B5 --actor test
    lc show-batch b-next
}
case_land() {
    lc event batch b-open --expect OPEN --version 1 --actor test --kind '{"CiStarted":{"run":"r2"}}'
    lc event batch b-open --expect CI_RUNNING --version 2 --actor test --kind '"Green"'
    lc land b-open --expect GREEN --version 3 --actor test --sha M2
    lc show sp-delivering
    lc land b-open --expect GREEN --version 3 --actor test --sha M3
}
case_settle() {
    lc event batch b-open --expect OPEN --version 1 --actor test --kind '{"CiStarted":{"run":"r2"}}'
    lc event batch b-open --expect CI_RUNNING --version 2 --actor test --kind '"Red"'
    lc show-batch b-open
    lc settle b-open --expect ATTRIBUTING --version 3 --actor test --eject sp-delivering
    lc show sp-delivering
}
case_abandon_batch() { lc abandon-batch b-open --expect OPEN --version 1 --actor test --reason "stopped"; lc show-batch b-open; lc show sp-delivering; }
case_eject_member() { lc eject-member b-open --bead-id sp-delivering --expect OPEN --version 1 --actor test --reason "red"; lc show sp-delivering; }
case_requeue_orphans() { lc requeue-orphans --actor test; }

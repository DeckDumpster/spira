# Landing-path parity

One row per landing-path mechanism, one column per land mode. A cell is exactly one of:

- `test:<file>::<name>` — the test proving the mechanism on that mode. The file must exist and contain `<name>`.
- `n/a:<reason>` — why the mechanism does not apply to that mode.
- `gap:<bead>` — the mechanism is missing on that mode; the bead (open, not yet landed) owns the outcome.

`spira/test-landing-parity.sh` fails on an empty cell, a `test:` that names nothing real, an `n/a:` with no reason and
a bare `gap`. `PARITY_STRICT=1` fails on every `gap:` as well. Add a mechanism or a mode by adding the row or column; `spira/suites.sh list` shows where the checker runs.

| mechanism | queue.local | queue.forge | pr | push |
|---|---|---|---|---|
| sift screening | test:sift/src/tests.rs::a_moved_tip_is_screened_again_and_judged_on_its_own | test:sift/src/tests.rs::a_moved_tip_is_screened_again_and_judged_on_its_own | n/a:no round pool to screen | n/a:no round pool to screen |
| known-red tip refusal | test:batcher-cut/src/io.rs::a_tip_a_round_ejected_red_is_not_in_the_round_pool_but_a_new_one_is | test:lifecycle/src/bead.rs::a_known_red_tip_is_neither_delivered_nor_requeued_to_submitted | n/a:no round ejects a member, so no red tip is recorded | n/a:no round ejects a member, so no red tip is recorded |
| provisional-member gate-red eject | n/a:the round's own full suite certifies a SUBMITTED member; admission refuses a gate-FAIL tip | test:queue/src/tests.rs::verdict_ejects_a_provisional_member_whose_gate_went_red_naming_the_gate_lines | n/a:no batch admits a provisional member | n/a:no batch admits a provisional member |
| eject names its evidence | test:batcher-cut/src/drive.rs::every_owner_is_ejected_with_its_suites_and_the_survivors_rerun_only_those | test:queue/src/tests.rs::verdict_ejects_a_provisional_member_whose_gate_went_red_naming_the_gate_lines | n/a:nothing is ejected from a batch | n/a:nothing is ejected from a batch |
| mender summons on a red suite | test:queue/src/tests/round.rs::each_red_suite_files_one_mender_lane_bead_carrying_its_output_pass_and_batch | test:queue/src/tests/verdict.rs::a_red_forge_batch_summons_a_mender_per_red_suite_once | n/a:no certification round runs suites | n/a:no certification round runs suites |
| pass telemetry (cert-pass records) | test:round-vm/src/run.rs::every_pass_of_a_round_yields_exactly_one_record_with_its_cause | test:queue/src/tests/verdict.rs::a_judged_forge_run_leaves_one_cert_pass_row_and_a_reread_leaves_no_second | n/a:no certification pass runs | n/a:no certification pass runs |
| stale-red / back-pressure handling | test:batcher/src/core/tests.rs::open_batch_pr_prepares_rather_than_cuts | test:batcher/src/core/tests.rs::open_batch_pr_prepares_rather_than_cuts | n/a:one PR per bead, no batch queue to back up | n/a:a push lands at once, no queue to back up |
| freeze / maintenance-window honouring | test:batcher-cut/src/main.rs::the_switch_holds_local_rounds_and_forge_batches_alike | test:batcher-cut/src/main.rs::the_switch_holds_local_rounds_and_forge_batches_alike | gap:sp-7y03a4.5 | gap:sp-7y03a4.6 |
| landing verification (closed is not landed) | test:queue/src/tests.rs::land_local_with_spira_lc_unreachable_lands_nothing | test:queue/src/tests/verdict.rs::a_merged_pr_lands_the_batch_with_the_merge_commit_and_a_second_pass_is_a_no_op | test:landing-pass/src/tests.rs::a_branch_gone_mid_pass_is_never_evidence_of_unlanded_work | test:landing-pass/src/tests.rs::a_branch_gone_mid_pass_is_never_evidence_of_unlanded_work |

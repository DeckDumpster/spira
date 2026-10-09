# Test plan — Configuration, bead store and preflight (`config-store-preflight`)

Area id `config-store-preflight`; use-case ids are `UC-config-store-preflight-NN`. The machine-checked catalogue is `config-store-preflight.toml`; this page is the prose.

How a process finds its world, and whether that world is sound.

## Intent

Every harness process resolves each setting with the precedence environment > spira.conf > derived default; non-prod instances never share prod's database, runtime directory or unit names. The repo-map and persona files turn configuration into behaviour: rows parse exactly, `repo:` labels resolve, persona partitions stay disjoint, and a non-prod instance refuses repositories outside its workspace or with network remotes. The bead store is provisioned in server mode, carries the Spira schema additions, and is never used with a `bd` whose migration count disagrees. Failure to read something is a refusal, never a silent pass. A push of the store is reported only when the remote head proves it.

## Use cases

| Group | UCs | Tier | Suites |
|---|---|---|---|
| A. Configuration resolution | 01–08 | T1 (14 is T0) | test-conf, test-configure, test-cockpit-runtime-path, test-freshclone, test-bin-manifest, test-watchd-unit-name |
| B. Store binding and schema guard | 09–11 | T1 | test-bd-resolve, test-bd-lock-retry, test-bd-schema-stamp |
| C. Repo-map, labels, partitions, containment | 12–18 | T0–T1 | test-repo-map, test-chamber-repo-labels, test-builder-qa-proposed, test-czar-partition, test-containment, test-id-prefix |
| D. Schema declaration and application | 19–22 | T1–T3 | test-schema, test-schema-apply |
| E. Store provisioning and push | 23–26 | T1–T2 | test-install-dolt-port-wait, test-install-dolt-breaker, test-beads-push-verify, test-beads-push-commit |
| F. Health (formerly preflight) | 27–34 | T0–T2 | test-freshclone (33); the rest are the Rust doctor crate |

## Verdicts applied and not applied

This page lands the catalogue and the `# tier:` / `# covers:` tags on the surviving suites. The plan was written against the bash `doctor.sh` and `conf.sh`; the Rust cutover changed what remains:

- **Doctor rows (27–32, 34): superseded.** The bash `test-doctor-*.sh` suites are gone; doctor is a Rust crate whose tests are `doctor/src/tests.rs`. Those UCs are marked uncovered (not deleted) until a suite declares them.
- **Config parsing (02, repo-map readers, fayth readers):** moved to `spira-config`; its tests are Rust. UC-02 stays an explicit gap (a malformed or inline-commented `spira.conf` row).
- **S1 done (UC-09/11):** `spira_config::env_bootstrap::schema_verdict` is the pure classifier behind `conf.sh`'s `check-bd`; its T1 table is `schema_verdict_table` in `spira-config/src/env_bootstrap.rs`.
- **Not done here, still open:** the merges of bd-resolve/bd-lock-retry into test-conf and of repo-label/lanes into one repo-map suite, test-bd-contract.sh (UC-25), the S5 containment injection and S6 source-time cost work, and the gaps G3–G12. They change production code or retire suites that other branches touch (`spira/conf.sh`, `spira/lib.sh` are in flight on other beads), so they are filed as follow-ups rather than done blind.
- **Missing suites** named by the old plan (test-roster-warn, test-schema-migration-guard, test-literal-lint, test-install-bd-init-cwd) no longer exist; UCs 17, 22 and 23 are marked uncovered.

## Gaps G3–G12

- **Closed by a test here:** G8 (`spira_require` in test-conf), G9 (build pin equals the `SPIRA_BD_TAG` default, in test-build-bd-release), G12 (symlink in/out of the workspaces root and `ssh://` / `git@` remotes, in test-containment).
- **Superseded by the Rust cutover:** G3 and G4 (the schema guard and config-file search moved to `spira-config`), G5–G7 (doctor is a Rust crate), and G11 (the answer-cursor consumers were deleted). Their tests belong in those crates; UC-02, 27–32 and 34 stay marked uncovered.
- **Left as stated:** G10 — test-schema-apply's server half still reports "NOT COVERED HERE" on stderr when no server fixture exists; the suite does not yet emit a TAP skip for it.

## Cost

No suite-seconds were measured: the old 640 s baseline predates the cutover and this change only edits header comments, so the after figure equals the before.

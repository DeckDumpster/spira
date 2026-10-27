# Component reference

Conventions: `$SPIRA_RUN` is the runtime directory, `$SPIRA_HOME` the `spira/` shell layer of
the active release, `<repo>` a repository from the registry. Binaries are resolved by bare name
on the release's `bin/`. All configuration comes from one `spira.toml` (`$SPIRA_TOML`), keys
registered in `spira/conf.d/`. A "shim" is a `spira/*.sh` file that sources `conf.sh` and
`exec`s the Rust binary of the same job.

Crates that are libraries only: `lifecycle` (pure state machines), `spira-otel`, `batcher`
(the round decisions; its I/O is `batcher-cut`), `migration-numbers`, `inbox-rules`,
`reconciler-engine` and `testkit` (a dev-dependency). `spira-world` is a library that builds
`world`, `dwarves` and `slay`.

- [Lifecycle and store](1-lifecycle-and-store.md)
- [Work intake and claiming](2-work-intake-and-claiming.md)
- [Dwarves and personas](3-dwarves-and-personas.md)
- [Gating, rounds and landing](4-gating-rounds-and-landing.md)
- [Releases and activation](5-releases-and-activation.md)
- [Observation](6-observation.md)
- [Operator surfaces](7-operator-surfaces.md)
- [Maintenance timers](8-maintenance-timers.md)
- [Shell layer](9-shell-layer.md)

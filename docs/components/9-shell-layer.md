## 4.9 Shell layer

`spira/` holds the shell that is still live beside the Rust: `conf.sh` and `conf.d/` (the
config registry), `lib.sh` (shared library whose bead-store functions are one-line shims onto
`bdq`), the hooks in `spira/hooks/` (`aeon-fence.sh`, `pre-commit`, `reference-transaction`,
`session.sh`, concierge hooks), `czar-fence.sh`, `hold.sh` / `held.sh` / `holds.sh`,
`ready.sh`, `host-check.sh`, `testdb.sh`, `schema.sh`, and every `test-*.sh` suite.

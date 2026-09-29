# testkit — DESIGN

## Intent

A plain `cargo test` of every crate is deterministic on any host, the gate's included
(`gate_mode = unit` runs the touched crates' tests on the host, sp-2ghui). One class of
defect broke that in at least five crates at once, and each crate had grown its own copy
of the code that caused it:

**A test that writes a program and then execs it fails with ETXTBSY ("Text file busy",
os error 26) whenever another test thread forks while the write descriptor is open.** The
forked child holds a copy of that descriptor until it execs, and the kernel refuses to
exec a file that anything has open for writing. Test threads fork constantly: every
`Command` spawn, and a real `fork()` wherever a crate uses `pre_exec`. Measured on
2026-09-29: landing-pass `real_halt_finds_podman_and_testenv_on_its_path` about 1 in 400
runs; spira-claim `store_lookup_fetch` and `live_event_insert_is_bounded` in a single
workspace run; queue-watch lost `run_bd_bounds_the_retry` to the same race in round 121.

testkit is the one place that knows how to write an executable safely. It is a
dev-dependency only and never ships in a binary.

## Contract

`testkit::write_exe(path, body)`:

- Creates or replaces `path` with exactly `body`, mode 0755.
- **This process never holds a write descriptor on `path`.** A child (`sh -c 'cat > "$1"'`)
  writes it, so nothing another thread forks can inherit a descriptor on it. The mode is
  set afterwards with chmod(2), which opens nothing.
- Returns after the writer has exited, so the file is complete and exec-able.
- Panics if the file cannot be written. It is test scaffolding, and a fixture that fails
  to build must fail the test that needed it.

Programs it runs: `sh` and `cat`, both on deps-lint's system allowlist.

## Non-goals

- No temp-directory management. Each crate keeps its own scratch layout.
- No retrying an exec that hit ETXTBSY. Retrying would hide the race, and this removes it.

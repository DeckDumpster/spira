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

**A test that makes a scratch directory under the system temp dir and never removes it
leaks it for good** (sp-qgfdi). Every crate had grown its own `fn dir(tag) -> PathBuf`
that called `std::env::temp_dir().join(..)` and `create_dir_all`, and almost none removed
what it made. One `cargo test --workspace` left 157 entries in /tmp; landing-pass alone had
left 84,964 before sp-kqbff. /tmp is a tmpfs with a fixed inode budget, and it ran out on
2026-09-29 during a stress loop. With `gate_mode = unit` running cargo tests on the host at
every gate, that is a disk-full outage on a timer.

testkit is the one place that knows how to write an executable safely, and the one place a
test gets a scratch directory from. It is a dev-dependency only and never ships in a
binary.

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

`testkit::TempDir::new(tag)`:

- Creates a fresh, empty directory `<temp_dir>/<tag>-<pid>-<n>`, where `<n>` is a
  process-wide counter, so two calls never share a directory, even with the same tag in
  parallel test threads. The tag keeps a leaked directory attributable by name. The path is
  canonical (symlinks in the temp dir resolved), so it compares equal to what `git` or
  `pwd -P` report for it.
- **Removes the directory and everything under it when the value is dropped**, and a panic
  unwinding out of a failing test drops it too. Nothing a test writes under it outlives
  the test.
- Derefs to `Path`, and is `AsRef<Path>` and `AsRef<OsStr>`, so `d.join(..)`,
  `Command::new(..).arg(&d)` and `.current_dir(&d)` read as they would on a `PathBuf`.
- `path()` borrows it; `to_path_buf()` copies the path out, and the copy does **not** keep
  the directory alive: whatever needs the directory must hold the `TempDir`.
- Panics if the directory cannot be made, for the same reason `write_exe` does.

**The rule that holds every crate to it:** spira-lint's `tmp-leak` refuses a call to
`temp_dir()` in test code anywhere outside testkit (see spira-lint/DESIGN.md). Test code
that wants scratch space takes a `TempDir`; there is no second helper to forget to clean.

## Non-goals

- No layout inside the directory. Each crate keeps its own scratch layout under it.
- No `keep()` that disarms the cleanup. A test that wants to inspect a failure's files
  can print them before it asserts.
- Temp files and dirs made by the code under test (a production `mktemp`, a script's
  `$TMPDIR`) are that code's to clean. A test that runs such code points `TMPDIR` into
  its own `TempDir`, and the drop takes them too.
- No retrying an exec that hit ETXTBSY. Retrying would hide the race, and this removes it.

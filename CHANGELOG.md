# Changelog

## 0.3.0 - 2026-11-11

- **With routing, a mutant's time limit comes from the tests it runs.** The limit
  was `max(20 s, 5 x the whole baseline)` for every mutant, although a routed
  mutant runs only the tests that reach it: in a crate with about 900 mutants and
  a baseline of 23 s, each of 4 mutants in an endless loop took 116 s. A routed
  mutant now gets `max(10 s, 5 x its tests)`, and never more than the old limit.
  "Its tests" is the sum of what each test it runs took alone during tracing,
  and the baseline time of a binary that runs whole. `--timeout`, `--no-routing`,
  the mutants built on their own and a mutant that no single test reaches are
  unchanged. A mutant that makes its tests more than five times slower, and
  longer than 10 s, without hanging is now a `timeout` where it could be caught
  or missed before; `--timeout` sets the limit for such a crate.
- **The progress and the report name the limit that was used.** The first line
  says `a mutant gets at most <n>s` when the limit is per mutant, the note of
  the second run says `did not end within their time limit`, and `statusReason`
  of a `Timeout` in `report.json` has the limit of that mutant.
- **Changed default: `--jobs` is half the processors (at least 1, at most 8)
  when routing is on.** It was a quarter (at most 4), which is one job on a
  machine with 4 to 7 processors, and it stays a quarter with `--no-routing`.
  The jobs have a process group and a `TMPDIR` of their own but share the
  working directory of the package: for tests that write files there or bind a
  fixed port, give `-j 1`, as before on machines with 8 or more processors.
- A mutant that reaches its limit with two or more jobs is still run again
  alone, with the limit it had. That is twice the limit for a mutant that really
  hangs, which is now seconds.
- A process group id of 1 or less is never signalled.

## 0.2.0 - 2026-11-10

- **Breaking: a `--file` glob that matches no mutants fails the run.** A glob in
  `examine_globs` of `.cargo/mutants.toml` is checked the same way, and the error
  says which of the two a glob came from. The check is made against the whole
  tree, before `--in-diff`; `--list` and `--build-only` do not run it. A glob that
  matches only excluded files, or only files that produce no mutants, is empty.
  The run names the glob, clears the report of an earlier run, runs nothing and exits with 1, where it used to pass with zero
  mutants (a renamed file left its module untested with CI green). `--allow-empty`
  restores the old behavior.
- **The temporary directory of the tests is outside the repository.** `TMPDIR`
  for the test processes was `target/schemata/tmp`, inside the repository under
  test, so a test that expects its temporary directory to be outside git ended up
  in the repository above. It is now a directory of its own for each run below
  the temporary directory of the system, so `GIT_CEILING_DIRECTORIES` is no longer
  needed.
- **Read-only leftovers are cleaned.** Write permission is restored recursively
  before a temporary directory is deleted, so a mutant that was killed at the
  timeout cannot leave directories that fail the next run. The run's directory is
  removed when the run ends, and what an earlier version left in
  `target/schemata/tmp` is cleared.
- **The temporary directory moved to the filesystem of the system's temporary
  directory.** It may be a tmpfs or mounted `noexec`, and it is no longer on the
  filesystem of the repository (no rename or hard link across); set `TMPDIR` to
  choose another place. The directory is created with mode 0700, directories of
  runs that were killed are swept by the next run, and a directory that cannot be
  removed is reported on stderr.
- **A killed test no longer races the cleanup.** After a group of test processes
  is killed, the run waits (up to 2 s) until it is gone before the temporary
  directory is cleared, and a "directory not empty" error is the only one that is
  retried. Restoring permissions does not follow symbolic links.

## 0.1.0

The first release: mutation testing for Rust that builds the mutants once, as
schemata. Unix only (Linux and macOS).

- **One build for all mutants.** The sources are rewritten so that every
  mutation sits behind an `if` selected by an environment variable at run time,
  and the package is built once. A mutant that does not compile in that form is
  built on its own, and one that does not compile at all is reported as
  unviable.
- **The mutants of cargo-mutants 27.1.0.** Which mutants exist, how they are
  named and which code is skipped follow cargo-mutants 27.1.0;
  `cargo schemata --list --json` prints what `cargo mutants --list --json`
  does. `.cargo/mutants.toml`, `--file`, `--exclude`, `--re`, `--exclude-re`,
  `--package` and `--workspace` are there.
- **Routing and early end.** A trace of which test reaches which mutant decides
  what is run for it, and a mutant is caught at the first failed test.
  `--no-routing` runs every test for every mutant, as cargo-mutants does.
- **Time limits as in cargo-mutants.** `max(20 s, 5 x the baseline)` for the
  tests of one mutant, or `--timeout`. Every test binary runs in a process
  group of its own that is killed at the limit, on an interruption (SIGINT,
  SIGTERM, SIGHUP) and when the run ends early, so no process stays behind.
- **`--in-diff`**: only the mutants on lines that a diff adds or changes.
- **`--shard <k>/<n>` and `cargo schemata merge`**: a run in several parts, and
  the parts put together, refusing parts that are not all of one run.
- **`--build-only`**: build the mutants once and report how each was built.
- **Output**: `report.json` in the mutation-testing-elements schema, and the
  lists `caught.txt`, `missed.txt`, `timeout.txt`, `unviable.txt`.
- **Not there**: `--html`, reuse of the results of an earlier run, doctests,
  `--sharding`, Windows.

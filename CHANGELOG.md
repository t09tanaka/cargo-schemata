# Changelog

## 0.7.0 - 2026-11-28

- **Tracing uses the whole share.** Each trace process runs one test, but
  tracing ran only as many of them at a time as there were mutant jobs, each
  with the mutant jobs' thread count. On an 8-core runner (2 jobs of 4 threads)
  tracing 615 tests took 167.9 s, a third of a 500 s run, with 2 of 8 threads
  busy. A trace process now gets `RUST_TEST_THREADS=1` (or the user's
  `--test-threads N`), and share / that many run at a time (at least 1).
  `--jobs` and `CARGO_SCHEMATA_JOBS` still cap the mutant jobs exactly as
  before, but not tracing: tracing ends before any mutant runs, so its
  processes never share the machine with the jobs. On a 12-thread machine,
  the same 615 tests of a 107-mutant run were traced in 48.7 s instead of
  186.6 s, and the run took 430 s instead of 614 s, with the same result for
  every mutant.
- The `traced:` line prints the number of trace workers.
- **What can differ:** a test that passes alone but fails when more trace
  processes run at the same time now makes its binary run whole for every
  mutant. That changes the plan, not the outcome: each mutant's result is the
  same.
- **Changed behaviour:** with one package selected (run inside a workspace
  member, or `-p <member>`), the member's own `.cargo/mutants.toml` now
  applies; before, only the workspace root's file was read and a member's
  `exclude_re` was silently ignored. Without a member file the root file is
  read as before, and `--config` always wins.
- **Changed behaviour:** with one package selected, a `--file` or `--exclude`
  glob (and `examine_globs` / `exclude_globs`) may now name a path relative to
  the package directory (`src/x.rs`) as well as to the workspace root
  (`crates/member/src/x.rs`). The listed and recorded paths stay relative to
  the workspace root, so reports, `merge` and parts do not change.

## 0.6.0 - 2026-11-22

- **`--sharding balanced`, and `balanced` is the new default.** The mutants
  that cannot be switched (const contexts, deleted struct fields and the like)
  are each built on their own, one after the other, and those builds were most
  of the time of a part: in one crate of about 3300 mutants in four parts,
  `round-robin` gave two parts 7 of them and two parts 5, and those two parts
  were the slow ones. `balanced` deals these mutants out in turn first, and the
  others continue the turn, so every part gets as many of them (at most one
  apart) and as many mutants in all. Which mutants these are follows from the
  source, so all the parts agree.
- **Changed behaviour:** without `--sharding`, a part gets other mutants than in
  0.4.0 and 0.5.0. Use one version and one `--sharding` for all the parts of a
  run, or pass `--sharding round-robin` to keep the old division.
- `config.shard.alone` in the report of a part lists the mutants built on their
  own. `cargo schemata merge` uses it for `balanced` parts, refuses a `balanced`
  part without it, and refuses parts whose lists differ.
- **A deleted match arm that leaves its `match` non-exhaustive is looked for
  among the deleted arms of that `match`.** rustc reports E0004 at the
  scrutinee, which is in no mutant, so the culprit was searched for among every
  deleted arm of the package, with a `cargo check` for each halving. Now only
  the deleted arms of the `match` whose scrutinee the error points at are
  candidates. In the crate above, the shared build of the four parts took 9,
  15, 3 and 8 check and build runs before, and 4, 6, 3 and 4 with both
  changes; the same mutants are unviable.
  Another `match` over the same type is worded the same by rustc, so while the
  culprit is halved out, an error counts as the same one only when it points
  into the `match` it was first seen at; a deleted arm that changes nothing is
  no longer taken for the culprit because the other `match` keeps failing.

## 0.5.0 - 2026-11-15

- **The default number of jobs and the test threads follow the share of the
  machine a run is given.** `--jobs` defaulted to a quarter of the processors
  whatever a CI runner had set, and every test process used all of them, so
  several shards on one machine each used every core and their baselines hit
  `--timeout`. Without `--jobs` the jobs now come from `CARGO_SCHEMATA_JOBS`,
  else from a quarter of the first of `RUST_TEST_THREADS`,
  `NEXTEST_TEST_THREADS` that holds a positive integer, else of
  the processors (at least 1, at most 4; `CARGO_BUILD_JOBS` is not read). Every test process, the baseline
  included, gets `RUST_TEST_THREADS` = share / jobs (at least 1; a
  `--test-threads N` after `--` is used instead, and then the default jobs are
  share / N), so jobs x threads stays within the share; `--jobs` or
  `CARGO_SCHEMATA_JOBS` beyond it print a warning.
  Without any variable the jobs are as before, but the tests of each process now
  use a share of the processors instead of all of them, so a baseline can take
  longer, and the limit derived from it with it. An empty
  `CARGO_SCHEMATA_JOBS` is unset; another invalid value exits with 2.

## 0.4.0 - 2026-11-11

- **A routed mutant starts its quickest test binary first.** A mutant that is
  reached by tests of several binaries ran them in the order cargo built them,
  so a mutant that a unit test catches in milliseconds could first wait for a
  slow integration test binary that passes. The binaries of a mutant are now
  ordered by what its tests in each of them took alone during tracing (the time
  in the baseline for a binary that runs whole); equal times keep cargo's
  order. A mutant that no single test reaches gets every binary, the quickest
  first. Within one binary nothing changes: since 0.2.0 the run ends at the
  first `test <name> ... FAILED` line, and libtest runs the tests it is given in
  the order of their names. Caught and missed mutants are the same as before; a
  missed mutant still runs all of its tests.
- A binary that does not end within the limit does not hide a failure in a
  binary that used to run before it: the binaries that come before it in cargo's
  order and have not run yet are run then, and that binary once more when
  others ran before it (with the limit anew), and the mutant is caught if one
  of them fails, a `timeout` otherwise. One result can differ
  from 0.3.0: a mutant whose quicker binary fails is now caught where a slower
  binary that came first and hangs made it a `timeout` (see "Where the result
  can differ" in the README).
- **`--sharding round-robin|slice`, and `round-robin` is the new default.**
  `--shard k/n` used to give part `k` a consecutive slice of the mutant list.
  Slow mutants sit together, so parts took very different times (the slowest of four parts of one crate of about 1000
  mutants took nearly twice as long as the fastest), and the slowest decided the
  time of the run. Round-robin gives mutant `i` of the list to part
  `i mod n`. `--sharding slice` (or `sharding = "slice"` in `.cargo/mutants.toml`,
  now used) is the old division, and what cargo-mutants does by default. The
  order of the list is the same on every machine, so all parts agree.
- **Changed behaviour:** without `--sharding`, a part gets other mutants than in
  0.3.0. Use one version and one `--sharding` for all the parts of a run, or pass
  `--sharding slice` to keep the old division.
- `config.shard.sharding` in the report of a part says how it was divided.
  `cargo schemata merge` refuses parts that were divided with different values
  (a 0.3.0 report has none and counts as `slice`), and checks that together the
  parts evaluate every mutant exactly once.

## 0.3.0 - 2026-11-11

- **With routing and two or more jobs, a mutant in an endless loop frees its
  job sooner.** Every mutant had `max(20 s, 5 x the whole baseline)` in the
  first run and again in the run alone that follows a time-out, although a
  routed mutant runs only the tests that reach it. The first run now gets
  `max(10 s, 5 x its tests)`, never more than the old limit. "Its tests" is the
  sum of what each test it runs took alone during tracing, and the baseline time
  of a binary that runs whole. A mutant that reaches it is run again alone with
  the old limit, and only that run decides: what is reported as caught, missed
  or `timeout` is what 0.2.0 reports. A mutant that hangs costs about 10 s plus
  the old limit instead of twice the old limit. Nothing changes with one job,
  `--timeout`, `--no-routing`, the mutants built on their own, or a mutant that
  no single test reaches.
- The note of the second run says `did not end within the time given for their
  own tests and are run again alone, with <n>s` when the first limit was the
  sooner one.
- The default of `--jobs` is unchanged (a quarter of the processors, at most 4).
  The jobs share the working directory of the package and the network, so a
  higher default could make tests fail, and a mutant look caught, in a package
  whose tests write files there or bind a fixed port. More jobs stay a choice.
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

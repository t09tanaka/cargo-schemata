# cargo-schemata

Mutation testing for Rust that builds mutants once, as schemata.

**Status: 0.4.0.** Listing the mutants (`--list`), building them (`--build-only`) and running the tests on them (a plain `cargo schemata`) work, and so do `--in-diff` (only the code a diff changes) and `--shard` with `merge` (a run in several parts). Unix only: Linux and macOS (Windows is not supported). There is no `--html`, and results of an earlier run are not reused (see "Differences from cargo-mutants").

## Goal

A replacement for cargo-mutants, run as `cargo schemata`. Instead of
rebuilding the project for every mutant, it rewrites the sources once,
embedding every mutation behind an `if` (a "mutation schema"), builds once,
and then runs the test binary in one process per mutant, selecting the active
mutation at runtime.

## How long it takes

On a command-line tool of ours (about 550 mutants, one of the tests waits a
little over two seconds), on a Mac with 12 cores:

| Tool                                | Time          |
| ----------------------------------- | ------------- |
| cargo-mutants 27.1.0                | about 80 min  |
| cargo-schemata, first run           | about 40 s    |
| cargo-schemata, second run          | about 25 s    |
| cargo-schemata with `--no-routing`  | about 7 min   |

The list of the four outcomes (caught, missed, timeout, unviable) was the same,
mutant by mutant. How it gets there: the sources are rewritten and built once
(see "Building"), a trace of which test reaches which mutant decides what runs
for each of them, and a mutant is caught at its first failed test (see
"Routing"). `--no-routing` turns the last two off and keeps the single build.

## Usage

```
cargo schemata [options] [-- <cargo test args> [-- <test binary args>]]
cargo schemata --list [--json] [options]
cargo schemata --build-only [--json] [options] [-- <cargo test args>]
cargo schemata merge <part.json>... [--output <dir>] [--json]
cargo schemata --version | --help
```

Run it in a package or workspace (or name one with `--dir`). `--list` prints
the mutants, one per line (`src/bin/tool.rs:2:15: replace + with - in main`).
With `--json` it prints the same array, with the same keys, as
`cargo mutants --list --json` (cargo-mutants 27.1.0). Only the comment in the
`diff` differs: `/* ~ changed by cargo-schemata ~ */`.

Which mutants exist, how they are named and which code is skipped follow
cargo-mutants 27.1.0: function bodies replaced by values of the return type,
binary operators, compound assignments and unary operators swapped or removed,
match arms deleted, match guards set to `true` and `false`, and struct fields
deleted when the literal has a `..base`. Skipped are unsafe functions, `fn new`
in an `impl`, `impl Default`, empty bodies, `#[test]` and `#[cfg(test)]` items,
`#[mutants::skip]`, and calls to `with_capacity`. Const functions and const or
static initializers are listed like any other code.

### Options

| Option                       | Meaning                                                                                  |
| ---------------------------- | ---------------------------------------------------------------------------------------- |
| `--list`                     | List the mutants                                                                         |
| `--build-only`               | Rewrite and build the mutants once, and report how each was built (see below)            |
| `--json`                     | Print JSON: the report of a run, or the list as `cargo mutants --list --json` does       |
| `-j`, `--jobs <n>`           | How many test processes run at a time. Default: a quarter of the processors, at least 1 and at most 4 |
| `--no-routing`               | Run every test for every mutant, and do not end a run at the first failed test (see "Routing") |
| `-t`, `--timeout <secs>`     | What one mutant's tests may take. Default: five times the time of the tests without a mutant, at least 20 seconds. With routing and two or more jobs, a first run is given up sooner and run again alone with this limit (see "Time limits") |
| `-o`, `--output <dir>`       | Where `report.json` and the `*.txt` lists are written. Default: `schemata.out`           |
| `-f`, `--file <glob>`        | Examine only files matching a glob; may be repeated. A glob with a `/` matches the whole path, one without matches a file or directory name anywhere |
| `--allow-empty`              | Do not fail when a `--file` glob, or an `examine_globs` glob of `.cargo/mutants.toml`, matches no mutants. Without it such a run exits with 1, names the glob (and whether it came from `--file` or the config file) and runs nothing, after clearing the report of an earlier run in `-o`, so that a renamed or removed file cannot leave its module untested while CI stays green. See [Empty globs](#empty-globs) |
| `-e`, `--exclude <glob>`     | Leave out files matching a glob; applied after `--file`                                  |
| `-F`, `--re <regex>`         | Examine only mutants whose name (as listed, with line and column) matches                |
| `-E`, `--exclude-re <regex>` | Leave out mutants whose name matches                                                     |
| `-D`, `--in-diff <file>`     | Only mutants on lines that the diff adds or changes (`-` reads standard input), see [Only the code a diff changes](#only-the-code-a-diff-changes---in-diff) |
| `--shard <k>/<n>`            | Run part `k` of `n` (`k` from 0) of the mutants; the report is then a part, see [A run in several parts](#a-run-in-several-parts---shard-and-merge) |
| `--sharding <method>`        | How `--shard` divides the mutants: `round-robin` (the default) or `slice`. Needs `--shard`; the same for every part |
| `-d`, `--dir <dir>`          | The crate or workspace directory. Default: `.`                                           |
| `--manifest-path <file>`     | The `Cargo.toml` of the package, instead of `--dir`                                      |
| `-p`, `--package <name>`     | Mutate only this package; may be repeated                                                |
| `--workspace`                | Mutate every package of the workspace                                                    |
| `--error <expr>`             | Also return this error value from functions returning `Result`                           |
| `--skip-calls <names>`       | Skip calls to these functions and methods (comma separated)                              |
| `--skip-calls-defaults <bool>` | Skip `with_capacity` as well. Default: true                                            |
| `--config <file>`, `--no-config` | Read another configuration file, or none                                             |
| `-- <cargo test args>`       | Which tests are built and run, as for `cargo test` (`--lib`, `--test it`, `--features x`). A second `--` starts the arguments of the test binaries (`--test-threads 1`). A bare word is a test name filter for the binaries. Ignored by `--list` |

### Empty globs

A glob in `--file` or in `examine_globs` of the config file is empty when no file it matches has a mutant that is left after the other filters. That includes a glob that matches only files given to `--exclude` or `exclude_globs`, and one that matches only files that produce zero mutants (no function worth mutating). The check is made against the whole tree, before `--in-diff` narrows the mutants: a diff that does not touch a module must not make the glob of that module fail, while a glob of a file that no longer exists fails whatever the diff is. `--list` and `--build-only` do not run the check: they print or build what the filters select, and an empty result is not an error there.

### `.cargo/mutants.toml`

The file is read like cargo-mutants does, and one file can serve both tools:

| Key                                                                                    | In cargo-schemata                                                                  |
| -------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `examine_globs`, `exclude_globs`, `examine_re`, `exclude_re`, `skip_calls`, `skip_calls_defaults`, `error_values` | Used: added to the command line (lists are joined, the command line first) |
| `sharding` | Used: `"round-robin"` or `"slice"`; `--sharding` wins |
| `additional_cargo_args`, `additional_cargo_test_args`, `all_features`, `build_timeout_multiplier`, `cap_lints`, `copy_vcs`, `copy_target`, `gitignore`, `features`, `minimum_test_timeout`, `no_default_features`, `output`, `profile`, `test_package`, `test_workspace`, `timeout_multiplier`, `test_tool` | Known to cargo-mutants 27.1.0 but not used: accepted, with a warning on stderr (`cargo schemata: warning: <file>: `timeout_multiplier` is not used by cargo-schemata`). Pass cargo arguments after `--`, and use `-j` and `--timeout` |
| any other key                                                                          | Refused, as cargo-mutants does: exit code 2, and the message names the key         |

A misspelt key that was ignored would be a configuration that says less than was meant.

See [Running the tests](#running-the-tests) for what a run prints and its exit codes.

### Only the code a diff changes (`--in-diff`)

`--in-diff <file>` takes a unified diff (what `git diff` prints; `-` reads it from standard input) and keeps the mutants whose span has a line that the diff adds or changes, or that is next to a line it deletes. Context lines do not count, and changes to files that are not Rust source are ignored. It works with `--list`, a run and `--build-only`, and comes before `--shard`.

```bash
git diff origin/main... > pr.diff
cargo schemata --list --in-diff pr.diff     # which mutants the change has
cargo schemata --in-diff pr.diff            # test only those
git diff origin/main... | cargo schemata --in-diff -
```

The file names in the diff are matched with the paths of the mutants, relative to the workspace root (`src/lib.rs`, a `b/` prefix is removed), so run `git diff` at the root of the workspace. The new side of the diff has to be what the tree has: a diff that is out of date is refused. This follows `cargo mutants --in-diff` (27.1.0), which the tests compare with.

| Diff                                                              | Result                                        |
| ----------------------------------------------------------------- | --------------------------------------------- |
| empty, changes no Rust file, or touches no mutant                 | Nothing to test. `--list` prints nothing; a run writes a report without mutants. Exit code 0 |
| its new side differs from the source files of the tree            | Refused, naming the first line. Exit code 5   |
| cannot be read or parsed                                          | Refused. Exit code 6                          |

### A run in several parts (`--shard` and `merge`)

`--shard <k>/<n>` runs part `k` of `n` (`k` from 0) of the mutants, so that `n` machines can each take one. After `--in-diff` and the other filters, the mutants are divided by `--sharding` (or `sharding` in `.cargo/mutants.toml`), with the names cargo-mutants uses:

- `round-robin` (the default): mutant `i` of the list goes to part `i mod n`. Mutants that are slow tend to sit together (the mutants of one file test the same slow code), and this spreads them over all the parts, so the parts take about as long. In one crate of about 1000 mutants in four parts, the slowest slice took nearly twice as long as the fastest, and the slowest part sets the time of the whole run.
- `slice`: consecutive ranges of the list, the first `ceil(total / n)` mutants to part 0 and so on (the last parts may be empty). This is the default of cargo-mutants.

The list is the one `--list` prints, in a fixed order that does not depend on the machine, the job or the number of jobs (the packages by name, then each package's files in the order the walk from its top files finds them, and the mutants of a file in source order; nothing is hashed or read from a directory listing), so for the same source tree and options every part computes the same division. Every part builds all the mutants it has to run and runs the tests without a mutant first, so a part costs a baseline and a build; what is divided is the test runs on the mutants.

**The default changed in 0.4.0** (it was `slice`). Parts made by 0.3.0 and by 0.4.0 are different divisions: give every job the same version and the same `--sharding`, or pass `--sharding slice` to get the old division back. `merge` refuses parts that were made with different values.

**The report of a part is not a result.** It lists every mutant of the run, and the mutants of the other parts have the status `Pending`. `config.shard` says which part it is (`index`, `count`, `sharding`, and the `order` of all mutants, which `merge` needs). A tool that judges a report must not pass a `Pending` mutant. The `*.txt` lists have the part's own mutants, the exit code is about them alone, and the last line says `shard k/n: 12 of 40 mutants`.

```bash
cargo schemata --shard 0/3 -o parts/0
cargo schemata --shard 1/3 -o parts/1
cargo schemata --shard 2/3 -o parts/2
cargo schemata merge parts/0/report.json parts/1/report.json parts/2/report.json -o schemata.out
```

`cargo schemata merge <part.json>... [-o <dir>] [--json]` reads the reports of all `n` parts of one run and writes what the run would have written without `--shard`: the same `report.json` (without `config`) and `caught.txt`, `missed.txt`, `timeout.txt` and `unviable.txt` (`duration` as each part measured it). It prints the counts and the missed mutants, and the exit code is 0 when no mutant was missed and 1 when one was. It runs nothing and needs no package. The order of the parts on the command line does not matter.

**It refuses parts that are not all the parts of one run**: exit code 1 and nothing written (the five output files of the last run are removed first, so that nothing old can be read as the result). That is: a file that is not the report of a part, a part that is missing or given twice, parts that were divided differently (`n`, or `--sharding`; a report of 0.3.0 has no `sharding` and counts as `slice`), parts whose mutants differ (another source tree, another set of options), and a part that evaluated a mutant of another part (together, the parts evaluate each mutant exactly once).

```
$ cargo schemata merge parts/0/report.json
cargo schemata merge: the parts are not the parts of one run
  - shard 1/3 is missing
  - shard 2/3 is missing
cargo schemata merge: FAILED (nothing was merged)
```

Use the same options for every part (`--in-diff`, `-f`, `-F`, `-p`, `--sharding`, the configuration file) and the same source tree.

#### In CI: one job per part

A GitHub Actions workflow that runs four parts side by side and merges them (`--in-diff` is left out; add it for the mutants of a pull request: `git diff origin/main... > pr.diff` in each job):

```yaml
name: mutation
on: pull_request
jobs:
  mutants:
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false # a part that finds a survivor must not stop the others
      matrix:
        shard: [0, 1, 2, 3]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-schemata --locked
      - run: cargo schemata --shard ${{ matrix.shard }}/4 -o parts/${{ matrix.shard }}
        continue-on-error: true # the verdict is the merge's; the report is what is needed
      - uses: actions/upload-artifact@v4
        with:
          name: part-${{ matrix.shard }}
          path: parts/${{ matrix.shard }}/report.json
          if-no-files-found: error
  merge:
    needs: mutants
    runs-on: ubuntu-latest
    steps:
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-schemata --locked
      - uses: actions/download-artifact@v4
        with:
          pattern: part-*
          path: parts
      - run: cargo schemata merge parts/*/report.json -o schemata.out # fails if a mutant was missed, or a part is missing
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: schemata.out
          path: schemata.out
```

A part that was killed or did not finish writes no report; `if-no-files-found: error` and `merge` (which lists the missing part) then fail the workflow.

### What each mutant carries

For the stages that follow, a mutant records, beyond the listed fields: its
position in the listing (`id`), the file, the function, the replaced region
(`edit_range`) and the syntactic unit around it (`target_range`) as byte
offsets into the file on disk, and a classification of where it sits: an
ordinary run-time expression, a const context, or a place where an `if`
cannot replace the code (the `&&` of a let chain, a deleted match arm or struct field). Only ordinary run-time
expressions can be embedded in a single build; the others have to be built one
at a time.

## Building (`--build-only`)

The package is copied (without `target/` and `.git`) to `<target dir>/schemata/work`, always the
same path, so that Cargo's incremental state is reused. Your source tree is never written to.
Every target's root file gets a small module, `__schemata`, that reads
`CARGO_SCHEMATA_MUTANT` once; mutants are `if`-switches on it:

- a function body: `if crate::__schemata::on(N) { return <value>; }` in front of the original body;
- an operator or a match guard: `(if on(N1) { mutant1 } else if on(N2) { mutant2 } else { original })`,
  with the other mutants of the operands inside each copy;
- a deleted match arm: `Pattern if !on(N) => ...`.

`cargo check --tests` then finds the mutants that do not compile; their error spans are mapped to
the mutant that produced them (bisecting when a span maps to none), those mutants are taken out,
and the rest is built once with `cargo test --no-run`. Mutants whose failure may come from the
switch itself (type mismatches and the like) and mutants that cannot be switched (const contexts,
let-chain `&&`, deleted struct fields) are built one at a time in `<target dir>/schemata/single-work`.
Builds use `--cap-lints=warn`, so lints like `unreachable_code` or `#![deny(warnings)]` do not
matter. `--build-only` prints the three groups (embedded, single, unviable) and the time of each
stage; `--json` adds the test binaries with the working directory and environment to run them in.

## Running the tests

A plain `cargo schemata` builds as described above, then:

1. **Baseline.** Every test binary is started once without a mutant. They have to
   pass: otherwise nothing is run, the end of the output of the failing binary is
   shown, and the exit code is 1. The time of the baseline sets the time limit.
2. **Routing** (unless `--no-routing`): every test is run once alone to learn
   which mutants it reaches, so that each mutant is run against those tests only.
   See "Routing" below.
3. **Each mutant.** The test binaries are started one after the other with
   `CARGO_SCHEMATA_MUTANT=<id>`, in the working directory and with the
   environment `cargo test` gives them. The first one that exits with a non-zero
   status (or is killed by a signal) *catches* the mutant, and the rest are not
   started (with routing, only the tests that reach the mutant are run). If all
   of them exit with 0 the mutant is *missed*. A mutant that
   did not compile is *unviable*. Standard input is closed and the output of the
   tests is discarded.
4. **Mutants built on their own** (const contexts, `&&` in let chains, deleted
   struct fields, and the ones whose type changes) are built and run one at a
   time, each right after its build: the test binaries and the `CARGO_BIN_EXE_*`
   binaries of a build are replaced by the next one. They run after the others,
   when nothing else is running.

What it prints (progress on stdout, or on stderr with `--json`):

```
built: 18 mutants in one build, 4 one at a time, 2 unviable (0.8s)
baseline: tests pass in 1.1s; a mutant gets 3s; 2 job(s) (1.1s)
traced: 12 tests run alone, 0 binar(ies) run whole (0.6s)
src/lib.rs:5:7: replace + with - in add ... caught in 0.0s
src/lib.rs:10:7: replace * with + in twice ... missed in 0.0s
src/lib.rs:17:11: replace += with *= in count_to ... timeout in 3.0s
src/lib.rs:24:18: replace + with * in join ... unviable in 0.0s
mutants: run in 2.4s
```

The parenthesized times are the stages: the build, the baseline, the tracing of
each test, and the run of the mutants.

When the run ends, the missed mutants, one line each, then the last line, both on
stderr, or on stdout when nothing is missed:

```
cargo schemata: 4 surviving mutant(s)
src/lib.rs:10:7: replace * with + in twice (missed)
cargo schemata: FAILED (17 caught, 4 missed, 1 timeout, 2 unviable in 13.1s)
```

| Exit code | Meaning                                                                                  |
| --------- | ---------------------------------------------------------------------------------------- |
| 0         | No mutant was missed (`cargo schemata: OK (...)`)                                        |
| 1         | A mutant was missed, a `--file` or `examine_globs` glob matches no mutants (unless `--allow-empty`), the tests fail without a mutant, something could not be built or run, or another run is at work in the package. For `merge`: a mutant was missed, or the parts are not all the parts of one run |
| 2         | Wrong usage: an unknown option, `--jobs 0`, a `--timeout` that is not positive, a `--shard` that is not `k/n` with `k < n`, an unknown `--sharding`, `--sharding` without `--shard`, an unknown key in `.cargo/mutants.toml`, options this version does not have (`--html`) |
| 5         | `--in-diff`: the diff is out of date with the source tree (the exit code of cargo-mutants) |
| 6         | `--in-diff`: the diff cannot be read or parsed                                           |
| 130 / 143 | Interrupted (SIGINT / SIGTERM). The test processes were killed first, and no report was written |

**There is no score threshold.** The exit code depends on the missed mutants alone.
`timeout` and `unviable` mutants do not fail the run; the counts show them.

### Routing

Running every test for every mutant wastes most of the time: one slow test, or
a test suite of a few hundred tests, is paid for again by each of hundreds of
mutants. All mutants are in one build, so which tests reach a mutant's code
does not depend on the mutant. The `__schemata` module can record it: with
`CARGO_SCHEMATA_TRACE=<file>` set, `on(N)` never selects a mutant and appends
`N` to the file, once per process and id (a bit set, one `O_APPEND` write;
child processes and several copies of the module in one process share the
file). Without the variable the module costs what it did.

1. The baseline run records the ids that the tests reach together.
2. Each test binary is asked for its tests (`<bin> <your filters> --list`) and
   every test is run alone (`<bin> --exact <name>`, `-j` at a time) with its own
   record. A mutant that a test reaches alone is run against that test.
3. A mutant is run with `<bin> --exact a b c`, the tests that reached it, and
   a binary none of whose tests reached it is not started. Your name filters and
   `--skip` only choose the tests that are listed; other arguments
   (`--test-threads`, `--nocapture`, `--ignored`) are kept. When several
   binaries have tests that reached it, the one whose tests took the least time
   during tracing is started first (a binary that runs whole counts with its
   time in the baseline), and the others only if it passes: a quick unit test
   that fails spares the mutant a slow integration test binary.
4. A mutant that no test reaches, even in the baseline, is not run: it is
   *missed*, `statusReason` of the report says `not reached by any test`, and the
   terminal says so (`missed (not reached by any test)`).
5. A mutant the baseline reaches but no test reaches alone (state that an
   earlier test set up) gets all the tests, the quickest binary first.

When a failed test is reported (`test <name> ... FAILED`), the processes of the
mutant are killed and the mutant is *caught*; libtest decides, so `should_panic`
and `catch_unwind` are handled. This is not done when the arguments change the
format or let tests write into it (`--format`, `-q`, `--color`, `--nocapture`, `--show-output`); routing still applies.

The line is also recognised with `--report-time` (`... FAILED <0.012s>`). The
output of the test binary is read as a whole, so a test that starts a child
process which inherits stdout and prints such a line itself (a test of a test
runner, say) ends the mutant early as *caught*, though no test of the crate
failed. With `--no-routing` nothing is watched, so this cannot happen there.

Limits: tracing is given up, with one line on stderr, when it takes more than
ten times the baseline (at least a minute); a binary whose output is not
libtest's (`harness = false` with its own `main`) or whose tests do not all
pass alone is always run whole; so is a binary when 80% or more of its tests
reach the mutant, or when the names would make a command line longer than 32 KiB.
If the baseline records nothing, routing is off.

**Where the result can differ from cargo-mutants**, and `--no-routing` gives
the old behaviour (every test, no early end):

- A test that only *writes* what another reads through a `static`, a file or an
  environment variable: the reader fails when the writer ran first in the same
  process, but run alone, only the writer reaches the mutant. The mutant is run
  against the writer alone, and may be missed.
- Tests whose path depends on randomness, time or a property-test seed: another
  run may not reach a mutant that the traced run reached, or the reverse.
- A child process of a test that inherits stdout and prints a line like
  `test x ... FAILED` ends the mutant early as caught (see above).
- A mutant that makes the tests hang after one of them has failed is a
  `timeout` without routing and *caught* with it. The same holds for two
  binaries: when the quicker one fails, the mutant is caught although a slower
  binary that cargo built first, and that would hang, comes before it without
  routing. The other way round nothing changes: when a binary does not end
  within the limit, the binaries that come before it in cargo's order and have
  not run yet are run, and then that binary once more if others ran before it
  and used part of its time; the mutant is caught if one of them fails.

### Time limits

A mutant's tests get `max(20 seconds, 5 x the baseline, rounded up to the second)`,
the rule of cargo-mutants 27.1.0, for all test binaries together; `--timeout`
replaces it. Only a mutant whose tests do not end within this limit is a
`timeout`, with or without routing.

With routing and two or more jobs, the *first* run of a mutant is given up
sooner, because a routed mutant runs only the tests that reach it:

```
first run = min(the limit above, max(10 seconds, 5 x its tests, rounded up to the second))
```

"Its tests" is what the plan for the mutant took without a mutant: for the tests
that are named on the command line, the sum of what each took when it ran alone
during tracing (each with the start of a process, so more than they take
together), and for a binary that runs whole, the time of that binary in the
baseline. A first run that reaches this is not a result: the mutant is run again
alone, after the others, with the limit above, and takes the status of that
run. So a mutant that is slow but ends is caught or missed exactly as it is with
one job or with `--no-routing`, and a mutant in an endless loop holds a job for
about 10 seconds instead of the whole limit before it gets its run alone.

The sooner limit is not used with one job (there is no second run, so the only
run gets the whole limit), with `--timeout` (every run gets exactly that), with
`--no-routing`, for the mutants built on their own, and for a mutant that no
single test reaches alone.

Every test binary is started in a process group of its own. When
the limit is reached the whole group gets SIGKILL, so processes the tests
started do not stay behind. The group is also killed when a test binary has
exited and left processes running. The mutant is a `timeout`, which does not
fail the run.

### Several mutants at a time

`-j <n>` runs `n` mutants at the same time, each in its own test processes with
its own `TMPDIR` (`<job>` in a directory of its own below the temporary directory of the system, `cargo-schemata-<pid>-...`; it is outside the repository, and it is removed when the run ends, read-only directories included; its mode is 0700, and the directories of runs that were killed (SIGKILL) are removed by the next run). The temporary directory of the system is a different filesystem from the repository on many machines: it may be a tmpfs (a small one fills up, and the memory it takes counts), may be mounted `noexec` (a test that runs a program it wrote there fails) and cannot be reached by a rename or hard link from the repository. Set `TMPDIR` for the run to point it elsewhere. The processes share the
working directory of the package copy: tests that write files there, or bind a
fixed port, can disturb each other, so give `-j 1` for such a package. The
default is a quarter of the logical processors, at least 1 and at most 4.

With `-j 2` or more, a mutant that reaches the limit of its first run may only
be slow because of the load, or because that limit was made from the time of its
own tests (see "Time limits"). Those are run again alone, with the limit of the
whole suite (or `--timeout`), after the others, and take the status of that
second run. The second run is what decides: a `timeout` does not fail the run,
so a mutant that was only slow must not end as one.

### The output

`--output <dir>` (default `schemata.out`) gets, after the run:

- `report.json`, in the [mutation-testing-elements JSON schema](https://github.com/stryker-mutator/mutation-testing-elements/tree/master/packages/report-schema),
  version 2. `framework.name` is `cargo-schemata`. Per file: `language` (`rust`),
  `source`, and per mutant `id` (`<path>#<number>`), `mutatorName` (the
  kind of mutation: `FnValue`, `BinaryOperator`, ...), `replacement`,
  `description` (the name `--list` shows), `location` (1-based, `end` exclusive),
  `status`, `duration` in milliseconds and, for a mutant that did not compile or
  timed out, `statusReason`. The statuses: caught is `Killed`, missed is
  `Survived`, timeout is `Timeout`, unviable is `CompileError`. `thresholds` is
  there because the schema requires it; there is no threshold.
- `caught.txt`, `missed.txt`, `timeout.txt`, `unviable.txt`: one mutant per line,
  named as `--list` names them, like the files of cargo-mutants.

A run starts by removing these five files (and only these) of the last run.
`--json` prints the contents of `report.json` on stdout.

### One run at a time

A run takes a lock on `<target dir>/schemata/lock` (`flock`, held by the process
for as long as it lives) before it changes anything. A second `cargo schemata`,
or `--build-only`, in the same package ends at once with exit code 1. A run that was
killed leaves no stale lock.

### Differences from cargo-mutants

Doctests are not run (only the test binaries `cargo test --no-run` builds).
`--html` and reuse of earlier results are not there. The `--sharding` default is `round-robin`, not `slice` as in cargo-mutants (see [A run in several parts](#a-run-in-several-parts---shard-and-merge)). The
test processes of the mutants that are built in share one working directory
(see `-j`).

## Tests

`tests/fixtures` holds small crates. `tests/fixtures/expected` is what
`cargo mutants --list` printed for them (`tests/fixtures/cases.json` lists the
invocations), and `cargo test` checks that `cargo schemata --list` prints the
same.

To rebuild the expected files (a new case in `cases.json`, or another version of
cargo-mutants), install the version the tests compare with and run the script from
the root of this repository, then commit what changed:

```bash
cargo install cargo-mutants --version 27.1.0 --locked
python3 scripts/gen-expected.py
```

`--in-diff` cases use the diff files in `tests/fixtures/diffs` (`@fixtures@` in a case
stands for their directory). `cargo test` also runs a fixture crate on all its parts and
merges them (`tests/shard_merge.rs`), which takes a few minutes the first time.

## License

MIT. See `LICENSE` and `NOTICE`.

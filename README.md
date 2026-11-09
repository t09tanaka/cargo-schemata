# cargo-schemata

Mutation testing for Rust that builds mutants once, as schemata.

**Status: under development.** Listing the mutants (`--list`), building them (`--build-only`) and running the tests on them (a plain `cargo schemata`) work. Not yet: `--in-diff`, `--shard`, `--html`, and results reused from an earlier run.

## Goal

A replacement for cargo-mutants, run as `cargo schemata`. Instead of
rebuilding the project for every mutant, it rewrites the sources once,
embedding every mutation behind an `if` (a "mutation schema"), builds once,
and then runs the test binary in one process per mutant, selecting the active
mutation at runtime.

## Usage

```
cargo schemata [options] [-- <cargo test args> [-- <test binary args>]]
cargo schemata --list [--json] [options]
cargo schemata --build-only [--json] [options] [-- <cargo test args>]
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
| `-t`, `--timeout <secs>`     | What one mutant's tests may take. Default: five times the time of the tests without a mutant, at least 20 seconds |
| `-o`, `--output <dir>`       | Where `report.json` and the `*.txt` lists are written. Default: `schemata.out`           |
| `-f`, `--file <glob>`        | Examine only files matching a glob; may be repeated. A glob with a `/` matches the whole path, one without matches a file or directory name anywhere |
| `-e`, `--exclude <glob>`     | Leave out files matching a glob; applied after `--file`                                  |
| `-F`, `--re <regex>`         | Examine only mutants whose name (as listed, with line and column) matches                |
| `-E`, `--exclude-re <regex>` | Leave out mutants whose name matches                                                     |
| `-d`, `--dir <dir>`          | The crate or workspace directory. Default: `.`                                           |
| `--manifest-path <file>`     | The `Cargo.toml` of the package, instead of `--dir`                                      |
| `-p`, `--package <name>`     | Mutate only this package; may be repeated                                                |
| `--workspace`                | Mutate every package of the workspace                                                    |
| `--error <expr>`             | Also return this error value from functions returning `Result`                           |
| `--skip-calls <names>`       | Skip calls to these functions and methods (comma separated)                              |
| `--skip-calls-defaults <bool>` | Skip `with_capacity` as well. Default: true                                            |
| `--config <file>`, `--no-config` | Read another configuration file, or none                                             |
| `-- <cargo test args>`       | Which tests are built and run, as for `cargo test` (`--lib`, `--test it`, `--features x`). A second `--` starts the arguments of the test binaries (`--test-threads 1`). A bare word is a test name filter for the binaries. Ignored by `--list` |

`.cargo/mutants.toml` is read like cargo-mutants does: `examine_globs`,
`exclude_globs`, `examine_re`, `exclude_re`, `skip_calls`,
`skip_calls_defaults` and `error_values` are added to the command line (lists
are joined, the command line first). Other keys are ignored.

See [Running the tests](#running-the-tests) for what a run prints and its exit codes.

### What each mutant carries

For the stages that follow, a mutant records, beyond the listed fields: its
position in the listing (`id`), the file, the function, the replaced region
(`edit_range`) and the syntactic unit around it (`target_range`) as byte
offsets into the file on disk, and a classification of where it sits: an
ordinary run-time expression, a const context, or a place where an `if`
cannot replace the code (the `&&` of a let chain, a deleted match arm or struct field). Only ordinary run-time
expressions can be embedded in a single build; the others have to be built one
at a time.

## Tests

`tests/fixtures` holds small crates. `tests/fixtures/expected` is what
`cargo mutants --list` printed for them (`tests/fixtures/cases.json` lists the
invocations), and `cargo test` checks that `cargo schemata --list` prints the
same.

## License

MIT. See `LICENSE` and `NOTICE`.

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
2. **Each mutant.** The test binaries are started one after the other with
   `CARGO_SCHEMATA_MUTANT=<id>`, in the working directory and with the
   environment `cargo test` gives them. The first one that exits with a non-zero
   status (or is killed by a signal) *catches* the mutant, and the rest are not
   started. If all of them exit with 0 the mutant is *missed*. A mutant that
   did not compile is *unviable*. Standard input is closed and the output of the
   tests is discarded.
3. **Mutants built on their own** (const contexts, `&&` in let chains, deleted
   struct fields, and the ones whose type changes) are built and run one at a
   time, each right after its build: the test binaries and the `CARGO_BIN_EXE_*`
   binaries of a build are replaced by the next one. They run after the others,
   when nothing else is running.

What it prints (progress on stdout, or on stderr with `--json`):

```
built: 18 mutants in one build, 4 one at a time, 2 unviable (0.8s)
baseline: tests pass in 1.1s; a mutant gets 3s; 2 job(s)
src/lib.rs:5:7: replace + with - in add ... caught in 0.0s
src/lib.rs:10:7: replace * with + in twice ... missed in 0.0s
src/lib.rs:17:11: replace += with *= in count_to ... timeout in 3.0s
src/lib.rs:24:18: replace + with * in join ... unviable in 0.0s
```

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
| 1         | A mutant was missed, the tests fail without a mutant, something could not be built or run, or another run is at work in the package |
| 2         | Wrong usage: an unknown option, `--jobs 0`, a `--timeout` that is not positive, options this version does not have (`--in-diff`, `--shard`, `--html`) |
| 130 / 143 | Interrupted (SIGINT / SIGTERM). The test processes were killed first, and no report was written |

**There is no score threshold.** The exit code depends on the missed mutants alone.
`timeout` and `unviable` mutants do not fail the run; the counts show them.

### Time limits

A mutant's tests get `max(20 seconds, 5 x the baseline, rounded up to the second)`,
the rule of cargo-mutants 27.1.0, for all test binaries together; `--timeout`
replaces it. Every test binary is started in a process group of its own. When
the limit is reached the whole group gets SIGKILL, so processes the tests
started do not stay behind. The group is also killed when a test binary has
exited and left processes running. The mutant is a `timeout`, which does not
fail the run.

### Several mutants at a time

`-j <n>` runs `n` mutants at the same time, each in its own test processes with
its own `TMPDIR` (`<target dir>/schemata/tmp/<job>`). The processes share the
working directory of the package copy: tests that write files there, or bind a
fixed port, can disturb each other, so give `-j 1` for such a package. The
default is a quarter of the logical processors, at least 1 and at most 4.

With `-j 2` or more, a mutant that reaches the limit may only be slow because of
the load. Those are run again alone, with the same limit, after the others, and
take the status of that second run.

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
`--in-diff`, `--shard`, `--html` and reuse of earlier results are not there. The
test processes of the mutants that are built in share one working directory
(see `-j`).

# Changelog

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

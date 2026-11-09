# cargo-schemata

Mutation testing for Rust that builds mutants once, as schemata.

**Status: under development.** Listing the mutants (`--list`) and building them (`--build-only`) work so far; running the tests does not.

## Goal

A replacement for cargo-mutants, run as `cargo schemata`. Instead of
rebuilding the project for every mutant, it rewrites the sources once,
embedding every mutation behind an `if` (a "mutation schema"), builds once,
and then runs the test binary in one process per mutant, selecting the active
mutation at runtime.

## Usage

```
cargo schemata --list [--json] [options]
cargo schemata --build-only [--json] [options] [-- <cargo args>]
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
| `--json`                     | Print JSON, as `cargo mutants --list --json` does                                        |
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
| `-- <cargo args>`            | Passed to `cargo check` and `cargo test --no-run` by `--build-only` (such as `--lib`); ignored by `--list` |

`.cargo/mutants.toml` is read like cargo-mutants does: `examine_globs`,
`exclude_globs`, `examine_re`, `exclude_re`, `skip_calls`,
`skip_calls_defaults` and `error_values` are added to the command line (lists
are joined, the command line first). Other keys are ignored.

Exit codes: 0 on success, 2 for wrong usage (including options this version
does not have, such as `--in-diff`, `--shard` and `-j`), 1 for other errors.

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

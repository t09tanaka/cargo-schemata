# cargo-schemata

Mutation testing for Rust that builds mutants once, as schemata.

**Status: under development.** Only listing the mutants works so far (`--list`).

## Goal

A replacement for cargo-mutants, run as `cargo schemata`. Instead of
rebuilding the project for every mutant, it rewrites the sources once,
embedding every mutation behind an `if` (a "mutation schema"), builds once,
and then runs the test binary in one process per mutant, selecting the active
mutation at runtime.

## Usage

```
cargo schemata --list [--json] [options] [-- <test args>]
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
| `--list`                     | List the mutants. Required for now: nothing else is implemented yet                      |
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
| `-- <test args>`             | Accepted and ignored by `--list`                                                         |

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

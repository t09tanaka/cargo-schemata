# cargo-schemata

Mutation testing for Rust that builds mutants once, as schemata.

**Status: under development.** Nothing is usable yet.

## Goal

A replacement for cargo-mutants, run as `cargo schemata`. Instead of
rebuilding the project for every mutant, it rewrites the sources once,
embedding every mutation behind an `if` (a "mutation schema"), builds once,
and then runs the test binary in one process per mutant, selecting the active
mutation at runtime.

## License

MIT. See `LICENSE` and `NOTICE`.

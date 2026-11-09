//! cargo-schemata: mutation testing for Rust (work in progress).

const HELP: &str = concat!(
    "cargo-schemata ",
    env!("CARGO_PKG_VERSION"),
    "\nMutation testing for Rust that builds mutants once, as schemata.\n\n",
    "USAGE:\n    cargo schemata [OPTIONS]\n\n",
    "OPTIONS:\n    -h, --help       Print help\n    -V, --version    Print version\n",
);

/// Drops the `schemata` token cargo inserts when invoked as `cargo schemata`.
fn strip_subcommand(args: Vec<String>) -> Vec<String> {
    let mut it = args.into_iter();
    let mut out: Vec<String> = it.next().into_iter().collect();
    let rest: Vec<String> = it.collect();
    let skip = usize::from(rest.first().map(String::as_str) == Some("schemata"));
    out.extend(rest.into_iter().skip(skip));
    out
}

fn run(args: &[String]) -> Result<String, String> {
    match args.get(1).map(String::as_str) {
        Some("--version" | "-V") => Ok(format!("cargo-schemata {}\n", env!("CARGO_PKG_VERSION"))),
        None | Some("--help" | "-h") => Ok(HELP.to_string()),
        Some(other) => Err(format!("unknown argument: {other}\n\n{HELP}")),
    }
}

fn main() {
    let args = strip_subcommand(std::env::args().collect());
    match run(&args) {
        Ok(out) => print!("{out}"),
        Err(err) => {
            eprint!("{err}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn handles_cargo_subcommand_invocation() {
        let args = strip_subcommand(v(&["cargo-schemata", "schemata", "--version"]));
        assert_eq!(args, v(&["cargo-schemata", "--version"]));
        assert!(run(&args).unwrap().starts_with("cargo-schemata "));
        assert!(run(&v(&["x", "--help"])).unwrap().contains("USAGE"));
        assert!(run(&v(&["x", "--nope"])).is_err());
    }
}

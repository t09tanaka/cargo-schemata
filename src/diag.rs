//! Read the JSON messages of `cargo check` / `cargo test --no-run`.

use std::io::Cursor;
use std::ops::Range;

use cargo_metadata::diagnostic::{DiagnosticLevel, DiagnosticSpan};
use cargo_metadata::{Artifact, Message};

/// An error message from rustc, with where it points to in the code that was built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diag {
    pub code: Option<String>,
    pub message: String,
    /// Primary spans, as file name (relative to the build directory) and byte range.
    pub spans: Vec<(String, Range<usize>)>,
}

/// What identifies an error independent of where it is.
pub type ErrSig = (Option<String>, String);

impl Diag {
    pub fn sig(&self) -> ErrSig {
        (self.code.clone(), self.message.clone())
    }

    /// One line for reports.
    pub fn describe(&self) -> String {
        match &self.code {
            Some(c) => format!("{c}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

#[derive(Default)]
pub struct Parsed {
    pub errors: Vec<Diag>,
    pub artifacts: Vec<Artifact>,
}

/// The outermost call site of a span that comes from a macro expansion.
fn user_span(mut span: &DiagnosticSpan) -> &DiagnosticSpan {
    while let Some(exp) = &span.expansion {
        span = &exp.span;
    }
    span
}

pub fn parse_messages(stdout: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    for msg in Message::parse_stream(Cursor::new(stdout)).flatten() {
        match msg {
            Message::CompilerMessage(cm) => {
                let d = cm.message;
                if !matches!(d.level, DiagnosticLevel::Error | DiagnosticLevel::Ice) {
                    continue;
                }
                if d.message.starts_with("aborting due to")
                    || d.message.starts_with("could not compile")
                    || d.message.starts_with("For more information")
                {
                    continue;
                }
                let mut spans: Vec<(String, Range<usize>)> = d
                    .spans
                    .iter()
                    .filter(|s| s.is_primary)
                    .map(user_span)
                    .map(|s| {
                        (
                            s.file_name.clone(),
                            s.byte_start as usize..s.byte_end as usize,
                        )
                    })
                    .collect();
                spans.dedup();
                let diag = Diag {
                    code: d.code.map(|c| c.code),
                    message: d.message,
                    spans,
                };
                if !parsed.errors.contains(&diag) {
                    parsed.errors.push(diag);
                }
            }
            Message::CompilerArtifact(a) => parsed.artifacts.push(a),
            _ => {}
        }
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_errors_and_skips_summaries() {
        let line = r#"{"reason":"compiler-message","package_id":"path+file:///x#a@0.1.0","manifest_path":"/x/Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"a","src_path":"/x/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"rendered":"r","$message_type":"diagnostic","children":[],"code":{"code":"E0308","explanation":null},"level":"error","message":"mismatched types","spans":[{"byte_end":20,"byte_start":10,"column_end":1,"column_start":1,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":1,"line_start":1,"suggested_replacement":null,"suggestion_applicability":null,"text":[]}]}}"#;
        let summary = line
            .replace("mismatched types", "aborting due to 1 previous error")
            .replace("E0308", "E0000");
        let out = format!("{line}\n{line}\n{summary}\n");
        let p = parse_messages(out.as_bytes());
        assert_eq!(p.errors.len(), 1);
        assert_eq!(p.errors[0].code.as_deref(), Some("E0308"));
        assert_eq!(p.errors[0].spans, vec![("src/lib.rs".to_owned(), 10..20)]);
    }
}

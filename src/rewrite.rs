//! Rewrite a source file so that many mutants live in it at once, selected at
//! run time by the `CARGO_SCHEMATA_MUTANT` environment variable.
//!
//! A [`FilePlan`] is built once per file; [`FilePlan::render`] then produces
//! the text for any subset of the embedded mutants, and records where each
//! mutant's code ended up so that compiler diagnostics can be mapped back.
//!
//! Shapes:
//! - function body: `if crate::__schemata::on(N) { return <value>; }` is
//!   inserted after the opening brace, the original body stays after it;
//! - binary / unary operator, match guard:
//!   `(if on(N1) { m1 } else if on(N2) { m2 } else { original })`, operands
//!   that hold other mutants are rewritten inside every copy;
//! - match arm deletion: ` if !on(N)` is inserted after the pattern.

use std::collections::BTreeSet;
use std::ops::Range;

use syn::spanned::Spanned;

use crate::mutant::{Context, Genre, Mutant};
use crate::span::{LineColumn, LineIndex};

/// The environment variable that turns the runtime into a recorder: it names a
/// file, and the first time a mutant's code is reached the id is appended to it.
pub const TRACE_ENV: &str = "CARGO_SCHEMATA_TRACE";

/// Module appended to the root file of every target. `ids` is one more than
/// the highest mutant id; the recorder keeps one bit for each.
///
/// With neither variable set, or with `CARGO_SCHEMATA_MUTANT=N`, `on` costs one
/// read of a `OnceLock`. With `CARGO_SCHEMATA_TRACE=<file>` it never selects a
/// mutant: each id is appended (one line, `O_APPEND`) the first time it is asked
/// for in this process, so several copies of the module (a library linked into
/// test binaries) and child processes can share one file.
pub fn runtime_module(ids: usize) -> String {
    let words = ids.div_ceil(64).max(1);
    format!(
        "
#[doc(hidden)]
#[allow(dead_code, unused)]
pub(crate) mod __schemata {{
    struct Trace {{
        file: ::std::fs::File,
        seen: ::std::vec::Vec<::std::sync::atomic::AtomicU64>,
    }}
    struct State {{
        selected: Option<u32>,
        trace: Option<Trace>,
    }}
    fn init() -> State {{
        let selected = ::std::env::var(\"CARGO_SCHEMATA_MUTANT\")
            .ok()
            .and_then(|s| s.parse().ok());
        let trace = ::std::env::var_os(\"CARGO_SCHEMATA_TRACE\")
            .and_then(|path| {{
                ::std::fs::OpenOptions::new().append(true).create(true).open(path).ok()
            }})
            .map(|file| Trace {{
                file,
                seen: (0..{words}).map(|_| ::std::sync::atomic::AtomicU64::new(0)).collect(),
            }});
        State {{ selected, trace }}
    }}
    pub fn on(n: u32) -> bool {{
        static STATE: ::std::sync::OnceLock<State> = ::std::sync::OnceLock::new();
        let state = STATE.get_or_init(init);
        if let Some(trace) = &state.trace {{
            let bit = 1u64 << (n % 64);
            if let Some(word) = trace.seen.get((n / 64) as usize) {{
                if word.fetch_or(bit, ::std::sync::atomic::Ordering::Relaxed) & bit == 0 {{
                    let _ = ::std::io::Write::write_all(&mut &trace.file, format!(\"{{n}}\\n\").as_bytes());
                }}
            }}
            return false;
        }}
        state.selected == Some(n)
    }}
}}
"
    )
}

/// If a top-level rewritten expression would grow past this many bytes, its
/// outermost mutants are built one at a time instead.
const MAX_EXPANSION_BYTES: usize = 64 * 1024;

/// How a mutant is built.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    /// Embedded in the shared schemata build.
    Embed,
    /// Needs a build of its own.
    Single,
}

pub fn placement(m: &Mutant) -> Placement {
    match (m.context, m.genre) {
        (
            Context::Runtime,
            Genre::FnValue | Genre::BinaryOperator | Genre::UnaryOperator | Genre::MatchArmGuard,
        )
        | (Context::NoIfWrap, Genre::MatchArm) => Placement::Embed,
        _ => Placement::Single,
    }
}

/// Where the code of one mutant is in the rendered text.
#[derive(Clone, Debug)]
pub struct Branch {
    pub range: Range<usize>,
    pub id: usize,
}

/// The whole rewritten expression holding several mutants.
#[derive(Clone, Debug)]
pub struct Group {
    pub range: Range<usize>,
    pub ids: Vec<usize>,
}

#[derive(Debug, Default)]
pub struct Rendered {
    pub text: String,
    pub branches: Vec<Branch>,
    pub groups: Vec<Group>,
}

#[derive(Debug)]
struct WrapMut {
    id: usize,
    edit: Range<usize>,
    repl: String,
}

#[derive(Debug)]
struct Wrap {
    range: Range<usize>,
    muts: Vec<WrapMut>,
    children: Vec<usize>,
}

#[derive(Debug)]
struct Insert {
    pos: usize,
    id: usize,
    text: String,
    is_arm: bool,
}

#[derive(Debug)]
pub struct FilePlan {
    original: String,
    wraps: Vec<Wrap>,
    roots: Vec<usize>,
    inserts: Vec<Insert>,
}

/// Mutants that cannot be embedded, with the reason.
pub type Rejected = Vec<(usize, String)>;

impl FilePlan {
    /// Plan the embedding of `mutants` (all [`Placement::Embed`]) in `original`.
    pub fn new(original: &str, mutants: &[&Mutant]) -> (FilePlan, Rejected) {
        let mut rejected: Rejected = Vec::new();
        let mut inserts: Vec<Insert> = Vec::new();
        let mut by_range: std::collections::BTreeMap<(usize, usize), Vec<WrapMut>> =
            Default::default();
        for m in mutants {
            let id = m.id;
            let t = &m.target_range;
            if t.end > original.len() || m.edit_range.end > original.len() {
                rejected.push((id, "range outside file".into()));
                continue;
            }
            match m.genre {
                Genre::FnValue => {
                    if original.as_bytes().get(t.start) != Some(&b'{') {
                        rejected.push((id, "function body does not start with `{`".into()));
                        continue;
                    }
                    inserts.push(Insert {
                        pos: t.start + 1,
                        id,
                        text: format!(
                            " if crate::__schemata::on({id}) {{ return {}; }}",
                            m.replacement
                        ),
                        is_arm: false,
                    });
                }
                Genre::MatchArm => match arm_pattern_end(&original[t.clone()]) {
                    Some(off) => inserts.push(Insert {
                        pos: t.start + off,
                        id,
                        text: format!(" if !crate::__schemata::on({id})"),
                        is_arm: true,
                    }),
                    None => rejected.push((id, "cannot locate match arm pattern".into())),
                },
                _ => {
                    if m.edit_range.start < t.start || m.edit_range.end > t.end {
                        rejected.push((id, "edit outside target".into()));
                        continue;
                    }
                    by_range.entry((t.start, t.end)).or_default().push(WrapMut {
                        id,
                        edit: m.edit_range.clone(),
                        repl: m.replacement.clone(),
                    });
                }
            }
        }
        inserts.sort_by_key(|i| (i.pos, i.id));

        // Nest the wrapped ranges. Parents come before children in this order.
        let mut wraps: Vec<Wrap> = Vec::new();
        let mut roots = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut entries: Vec<((usize, usize), Vec<WrapMut>)> = by_range.into_iter().collect();
        entries.sort_by_key(|((s, e), _)| (*s, std::cmp::Reverse(*e)));
        for ((s, e), muts) in entries {
            while let Some(&top) = stack.last() {
                if wraps[top].range.end <= s {
                    stack.pop();
                } else {
                    break;
                }
            }
            if let Some(&top) = stack.last()
                && e > wraps[top].range.end
            {
                for m in muts {
                    rejected.push((m.id, "overlaps another mutated expression".into()));
                }
                continue;
            }
            let idx = wraps.len();
            wraps.push(Wrap {
                range: s..e,
                muts,
                children: Vec::new(),
            });
            match stack.last() {
                Some(&top) => wraps[top].children.push(idx),
                None => roots.push(idx),
            }
            stack.push(idx);
        }
        let mut plan = FilePlan {
            original: original.to_owned(),
            wraps,
            roots,
            inserts,
        };
        for root in plan.roots.clone() {
            plan.limit_expansion(root, &mut rejected);
        }
        (plan, rejected)
    }

    /// Estimated rendered size of a node with all of its mutants.
    fn cost(&self, n: usize) -> usize {
        let w = &self.wraps[n];
        let len = w.range.len();
        let inner: usize = w
            .children
            .iter()
            .map(|&c| self.cost(c).saturating_sub(self.wraps[c].range.len()))
            .sum();
        let copies = if w.muts.is_empty() {
            1
        } else {
            w.muts.len() + 1
        };
        copies.saturating_mul(len.saturating_add(inner))
    }

    fn limit_expansion(&mut self, n: usize, rejected: &mut Rejected) {
        if self.cost(n) > MAX_EXPANSION_BYTES && !self.wraps[n].muts.is_empty() {
            for m in std::mem::take(&mut self.wraps[n].muts) {
                rejected.push((m.id, "rewriting would grow the code too much".into()));
            }
        }
        for c in self.wraps[n].children.clone() {
            self.limit_expansion(c, rejected);
        }
    }

    /// Ids of all arm-deletion mutants (used to narrow non-exhaustive match errors).
    pub fn arm_ids(&self) -> Vec<usize> {
        self.inserts
            .iter()
            .filter(|i| i.is_arm)
            .map(|i| i.id)
            .collect()
    }

    /// Render the file with only the mutants in `active` embedded.
    pub fn render(&self, active: &BTreeSet<usize>) -> Rendered {
        let mut st = Rendered {
            text: String::with_capacity(self.original.len() + 256),
            ..Rendered::default()
        };
        self.emit(&mut st, active, 0, self.original.len(), &self.roots);
        st
    }

    fn emit(
        &self,
        st: &mut Rendered,
        active: &BTreeSet<usize>,
        from: usize,
        to: usize,
        nodes: &[usize],
    ) {
        let mut cur = from;
        for &n in nodes {
            let r = &self.wraps[n].range;
            if r.start < cur || r.end > to {
                continue;
            }
            self.gap(st, active, cur, r.start);
            self.node(st, active, n);
            cur = r.end;
        }
        self.gap(st, active, cur, to);
    }

    fn gap(&self, st: &mut Rendered, active: &BTreeSet<usize>, from: usize, to: usize) {
        let first = self.inserts.partition_point(|i| i.pos < from);
        let mut cur = from;
        for ins in &self.inserts[first..] {
            if ins.pos >= to {
                break;
            }
            if !active.contains(&ins.id) {
                continue;
            }
            st.text.push_str(&self.original[cur..ins.pos]);
            cur = ins.pos;
            let start = st.text.len();
            st.text.push_str(&ins.text);
            st.branches.push(Branch {
                range: start..st.text.len(),
                id: ins.id,
            });
        }
        st.text.push_str(&self.original[cur..to]);
    }

    fn node(&self, st: &mut Rendered, active: &BTreeSet<usize>, n: usize) {
        let w = &self.wraps[n];
        let live: Vec<&WrapMut> = w.muts.iter().filter(|m| active.contains(&m.id)).collect();
        if live.is_empty() {
            self.emit(st, active, w.range.start, w.range.end, &w.children);
            return;
        }
        let group_start = st.text.len();
        st.text.push('(');
        for m in &live {
            let start = st.text.len();
            st.text
                .push_str(&format!("if crate::__schemata::on({}) {{ ", m.id));
            self.emit(st, active, w.range.start, m.edit.start, &w.children);
            st.text.push_str(&m.repl);
            self.emit(st, active, m.edit.end, w.range.end, &w.children);
            st.text.push_str(" }");
            st.branches.push(Branch {
                range: start..st.text.len(),
                id: m.id,
            });
            st.text.push_str(" else ");
        }
        st.text.push_str("{ ");
        self.emit(st, active, w.range.start, w.range.end, &w.children);
        st.text.push_str(" }");
        st.text.push(')');
        st.groups.push(Group {
            range: group_start..st.text.len(),
            ids: live.iter().map(|m| m.id).collect(),
        });
    }
}

/// Byte offset of the end of the pattern of a guard-less match arm, relative
/// to the arm text.
fn arm_pattern_end(arm_text: &str) -> Option<usize> {
    let arm: syn::Arm = syn::parse_str(arm_text).ok()?;
    if arm.guard.is_some() {
        return None;
    }
    let end = arm.pat.span().end();
    let index = LineIndex::new(arm_text);
    Some(index.offset(LineColumn {
        line: end.line,
        column: end.column + 1,
    }))
}

/// Replace `edit` in `original` with `replacement`: the text of one mutant
/// applied to an otherwise untouched file.
pub fn apply_single(original: &str, m: &Mutant) -> String {
    let mut s = String::with_capacity(original.len() + m.replacement.len());
    s.push_str(&original[..m.edit_range.start]);
    s.push_str(&m.replacement);
    s.push_str(&original[m.edit_range.end..]);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_pattern_end_finds_pattern() {
        let text = "Some(1) | None => foo(),";
        let off = arm_pattern_end(text).unwrap();
        assert_eq!(&text[..off], "Some(1) | None");
        assert!(arm_pattern_end("x if x > 1 => 2,").is_none());
        let text = "é(a) =>\n    a,";
        assert_eq!(&text[..arm_pattern_end(text).unwrap()], "é(a)");
    }

    fn mutant_for(code: &str, name_part: &str) -> Mutant {
        use crate::options::{Config, Filters, Options};
        use crate::visit::{SourceFile, walk_file};
        use crate::workspace::Package;
        use std::sync::Arc;
        let package = Arc::new(Package {
            name: "t".into(),
            relative_dir: "".into(),
            top_sources: vec![],
            all_sources: vec![],
        });
        let sf = SourceFile {
            package,
            path: "src/lib.rs".into(),
            original: Arc::new(code.to_owned()),
            code: Arc::new(code.to_owned()),
            is_top: true,
        };
        let options = Options::new(&Filters::default(), &Config::default()).unwrap();
        let (mutants, _) = walk_file(&sf, &[], &options).unwrap();
        mutants
            .into_iter()
            .find(|m| m.name(true).contains(name_part))
            .unwrap_or_else(|| panic!("no mutant {name_part}"))
    }

    fn all_mutants(code: &str) -> Vec<Mutant> {
        use crate::options::{Config, Filters, Options};
        use crate::visit::{SourceFile, walk_file};
        use crate::workspace::Package;
        use std::sync::Arc;
        let package = Arc::new(Package {
            name: "t".into(),
            relative_dir: "".into(),
            top_sources: vec![],
            all_sources: vec![],
        });
        let sf = SourceFile {
            package,
            path: "src/lib.rs".into(),
            original: Arc::new(code.to_owned()),
            code: Arc::new(code.to_owned()),
            is_top: true,
        };
        let options = Options::new(&Filters::default(), &Config::default()).unwrap();
        let (mut mutants, _) = walk_file(&sf, &[], &options).unwrap();
        for (i, m) in mutants.iter_mut().enumerate() {
            m.id = i;
        }
        mutants
    }

    #[test]
    fn nested_operators_are_wrapped_in_every_copy() {
        let code = "fn f(a: u32, b: u32, c: u32) -> u32 {\n    a + b * c\n}\n";
        let ms: Vec<Mutant> = all_mutants(code)
            .into_iter()
            .filter(|m| m.genre == Genre::BinaryOperator)
            .collect();
        assert_eq!(ms.len(), 4);
        let refs: Vec<&Mutant> = ms.iter().collect();
        let (plan, rejected) = FilePlan::new(code, &refs);
        assert!(rejected.is_empty());
        let active: BTreeSet<usize> = ms.iter().map(|m| m.id).collect();
        let r = plan.render(&active);
        // the inner `b * c` mutants appear in the outer's original and both mutant copies
        let id_inner = ms.iter().find(|m| m.replacement == "/").unwrap().id;
        assert_eq!(
            r.text.matches(&format!("on({id_inner})")).count(),
            3,
            "{}",
            r.text
        );
        // an empty active set reproduces the original
        assert_eq!(plan.render(&BTreeSet::new()).text, code);
        // every branch range is a mutant's `if ... { ... }`
        for b in &r.branches {
            assert!(r.text[b.range.clone()].starts_with("if crate::__schemata::on("));
        }
    }

    #[test]
    fn fn_body_and_arm_inserts() {
        let code = "fn f(x: Option<u8>) -> u8 {\n    match x {\n        Some(1) => 1,\n        _ => 2,\n    }\n}\n";
        let ms = all_mutants(code);
        let refs: Vec<&Mutant> = ms.iter().collect();
        let (plan, rejected) = FilePlan::new(code, &refs);
        assert!(rejected.is_empty(), "{rejected:?}");
        let active: BTreeSet<usize> = ms.iter().map(|m| m.id).collect();
        let r = plan.render(&active);
        assert!(
            r.text
                .contains("{ if crate::__schemata::on(0) { return 0; }")
        );
        assert!(
            r.text.contains("Some(1) if !crate::__schemata::on("),
            "{}",
            r.text
        );
    }

    #[test]
    fn single_applies_one_edit() {
        let code = "fn f(a: u32) -> u32 { a + 1 }\n";
        let m = mutant_for(code, "replace + with -");
        assert_eq!(apply_single(code, &m), "fn f(a: u32) -> u32 { a - 1 }\n");
    }
}

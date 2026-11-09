// Copyright 2021 - 2025 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! A mutation discovered in a source file: what it replaces, with what, and
//! the extra location information the rewriting stage needs.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use camino::Utf8PathBuf;
use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};
use similar::TextDiff;

use crate::span::Span;

/// Marker comment placed next to a replacement in the `diff` of `--list --json`.
pub const MUTATION_MARKER_COMMENT: &str = "/* ~ changed by cargo-schemata ~ */";

/// Various broad categories of mutants.
#[derive(Clone, Copy, Eq, PartialEq, Debug, Serialize)]
pub enum Genre {
    /// Replace the body of a function with a fixed value.
    FnValue,
    /// Replace `==` with `!=` and so on.
    BinaryOperator,
    /// Delete a unary `!` or `-`.
    UnaryOperator,
    /// Delete match arm.
    MatchArm,
    /// Replace the expression of a match arm guard with a fixed value.
    MatchArmGuard,
    /// Delete a field from a struct literal that has a base (default) expression.
    StructField,
}

/// Where a mutated region sits, which decides how a later stage can build it.
#[derive(Clone, Copy, Eq, PartialEq, Debug, Serialize)]
pub enum Context {
    /// An ordinary run-time expression: it can be wrapped in
    /// `if crate::__schemata::on(N) { mutant } else { original }`.
    Runtime,
    /// A const context (const fn, const/static initializer, array length,
    /// const generic argument, const block, enum discriminant): a run-time
    /// switch cannot be evaluated here.
    Const,
    /// A place where an `if` expression cannot replace the code: the `&&` of a
    /// let chain, a deleted match arm, a deleted struct field. (`genre` tells
    /// which; a later stage may still embed arms and fields some other way.)
    NoIfWrap,
}

/// The target of a mutation, providing additional context about what is being mutated.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum MutationTarget {
    /// A field in a struct literal expression.
    StructLiteralField {
        /// The name of the field being deleted.
        field_name: String,
        /// The name/type of the struct.
        struct_name: String,
    },
}

/// The function containing a mutant.
///
/// This is used for both mutations of the whole function, and smaller mutations within it.
#[derive(Eq, PartialEq, Debug, Serialize)]
pub struct Function {
    /// The function that's being mutated, including any containing namespaces.
    pub function_name: String,
    /// The return type of the function, including a leading "-> ".
    /// Empty if the function has no return type (i.e. returns `()`).
    pub return_type: String,
    /// The span (line/column range) of the entire function.
    pub span: Span,
}

/// A mutation applied to source code.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Mutant {
    /// Position in the enumeration, starting at 0.
    pub id: usize,
    /// Human-readable name, including file, location and change description.
    pub name: String,
    /// Name of the package that owns the file.
    pub package: String,
    /// Path of the file relative to the workspace root.
    pub file: Utf8PathBuf,
    /// The nearest enclosing function, if any.
    pub function: Option<Arc<Function>>,
    /// The replaced region, as 1-based line/column.
    pub span: Span,
    /// A shorter version of the text being replaced (the pattern of a match arm).
    pub short_replaced: Option<String>,
    /// The replacement text.
    pub replacement: String,
    /// What general category of mutant this is.
    pub genre: Genre,
    /// Additional context about what is being mutated.
    pub target: Option<MutationTarget>,
    /// The replaced region as byte offsets into the file as it is on disk.
    ///
    /// For an operator this is the operator, for a function body the
    /// statements between the braces.
    pub edit_range: Range<usize>,
    /// The syntactic unit that the mutation lives in, as byte offsets into the
    /// file on disk: the whole binary or unary expression, the whole function
    /// body block including braces, the match arm, the guard expression, or
    /// the struct field with its trailing comma. Always contains `edit_range`.
    pub target_range: Range<usize>,
    /// How a later stage can embed this mutant.
    pub context: Context,
    /// The file text with line endings normalized to `\n`, which `span` indexes.
    pub code: Arc<String>,
}

impl Mutant {
    /// Return text of the whole file with the mutation applied.
    pub fn mutated_code(&self) -> String {
        self.span.replace(
            &self.code,
            &format!("{} {}", &self.replacement, MUTATION_MARKER_COMMENT),
        )
    }

    /// Describe the mutant briefly, not including the location.
    ///
    /// The result is like `replace factorial -> u32 with Default::default()`.
    pub fn describe_change(&self) -> String {
        self.parts().concat()
    }

    pub fn name(&self, show_line_col: bool) -> String {
        let loc = if show_line_col {
            format!(":{}:{}: ", self.span.start.line, self.span.start.column)
        } else {
            ": ".to_owned()
        };
        format!("{}{}{}", self.file_slashes(), loc, self.describe_change())
    }

    /// The file path with forward slashes.
    pub fn file_slashes(&self) -> String {
        crate::path::to_slash_path(&self.file)
    }

    fn parts(&self) -> Vec<String> {
        let s = |x: &str| x.to_owned();
        let mut v: Vec<String> = Vec::new();
        match self.genre {
            Genre::FnValue => {
                v.push(s("replace "));
                let function = self
                    .function
                    .as_ref()
                    .expect("FnValue mutant should have a function");
                v.push(function.function_name.clone());
                if !function.return_type.is_empty() {
                    v.push(s(" "));
                    v.push(function.return_type.clone());
                }
                v.push(s(" with "));
                v.push(self.replacement.clone());
            }
            Genre::MatchArmGuard => {
                v.push(s("replace match guard "));
                v.push(squash_lines(&self.original_text()).into_owned());
                v.push(s(" with "));
                v.push(self.replacement.clone());
            }
            Genre::MatchArm => {
                v.push(s("delete match arm "));
                v.push(
                    squash_lines(
                        self.short_replaced
                            .as_ref()
                            .expect("short_replaced should be set on MatchArm"),
                    )
                    .into_owned(),
                );
            }
            Genre::StructField => {
                if let Some(MutationTarget::StructLiteralField {
                    field_name,
                    struct_name,
                }) = &self.target
                {
                    v.push(s("delete field "));
                    v.push(field_name.clone());
                    v.push(s(" from struct "));
                    v.push(struct_name.clone());
                    v.push(s(" expression"));
                } else {
                    v.push(s("delete field from struct expression"));
                }
            }
            _ => {
                if self.replacement.is_empty() {
                    v.push(s("delete "));
                } else {
                    v.push(s("replace "));
                }
                v.push(self.original_text());
                if !self.replacement.is_empty() {
                    v.push(s(" with "));
                    v.push(self.replacement.clone());
                }
            }
        }
        if !matches!(self.genre, Genre::FnValue)
            && let Some(func) = &self.function
        {
            v.push(s(" in "));
            v.push(func.function_name.clone());
        }
        v
    }

    pub fn original_text(&self) -> String {
        self.span.extract(&self.code)
    }

    /// Return a unified diff for the mutant.
    pub fn diff(&self, mutated_code: &str) -> String {
        let old_label = self.file_slashes();
        let new_label = self.describe_change().replace('\n', " ");
        TextDiff::from_lines(self.code.as_str(), mutated_code)
            .unified_diff()
            .context_radius(8)
            .header(&old_label, &new_label)
            .to_string()
    }

    /// Convert this mutant to a JSON value including the diff, in the shape of
    /// `cargo mutants --list --json`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::to_value(self).expect("Serialize mutant");
        obj.as_object_mut().unwrap().insert(
            "diff".to_owned(),
            serde_json::json!(self.diff(&self.mutated_code())),
        );
        obj
    }
}

impl Serialize for Mutant {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut ss = serializer.serialize_struct("Mutant", 7)?;
        ss.serialize_field("name", &self.name)?;
        ss.serialize_field("package", &self.package)?;
        ss.serialize_field("file", &self.file_slashes())?;
        ss.serialize_field("function", &self.function.as_ref().map(Arc::as_ref))?;
        ss.serialize_field("span", &self.span)?;
        ss.serialize_field("replacement", &self.replacement)?;
        ss.serialize_field("genre", &self.genre)?;
        ss.end()
    }
}

/// Combine multiple lines to one, removing indentation following a newline.
///
/// Newlines are replaced by a space, only if there is not already a trailing space.
pub fn squash_lines(s: &str) -> Cow<'_, str> {
    if s.contains('\n') {
        let mut r = String::new();
        let mut in_indent = false;
        for c in s.chars() {
            match c {
                ' ' | '\t' | '\n' if in_indent => (),
                '\n' => {
                    if !r.ends_with(' ') {
                        r.push(' ');
                    }
                    in_indent = true;
                }
                c => {
                    in_indent = false;
                    r.push(c);
                }
            }
        }
        Cow::Owned(r)
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod test {
    use super::squash_lines;

    #[test]
    fn squash() {
        assert_eq!(squash_lines("squash_lines a b c"), "squash_lines a b c");
        assert_eq!(squash_lines("a\n    b c \n\nd  \n  e"), "a b c d  e");
    }
}

// Copyright 2021 - 2025 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! Visit all the files in a source tree, and then the AST of each file,
//! to discover mutation opportunities.
//!
//! Walking the tree starts with the root files of each package target. The tree
//! walker then follows `mod` statements to recursively visit other files.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use proc_macro2::{Ident, TokenStream};
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, BinOp, Block, Expr, ExprPath, File, ItemFn, ReturnType, Signature, UnOp};

use crate::fnvalue::return_type_replacements;
use crate::mutant::{Context, Function, Genre, Mutant, MutationTarget};
use crate::options::Options;
use crate::path::ascent;
use crate::pretty::ToPrettyString;
use crate::span::{LineIndex, Span};
use crate::workspace::Package;

/// A source file that was read during discovery.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub package: Arc<Package>,
    /// Path relative to the workspace root.
    pub path: Utf8PathBuf,
    /// The text as on disk.
    pub original: Arc<String>,
    /// The text with `\r\n` normalized to `\n`; this is what gets parsed.
    pub code: Arc<String>,
    /// True if this is the root file of a target.
    pub is_top: bool,
}

impl SourceFile {
    fn load(
        tree_path: &Utf8Path,
        path: &Utf8Path,
        package: &Arc<Package>,
        is_top: bool,
    ) -> Result<Option<SourceFile>> {
        if ascent(path) > 0 {
            eprintln!("warning: skipping source outside of tree: {path}");
            return Ok(None);
        }
        let full_path = tree_path.join(path);
        let original = std::fs::read_to_string(&full_path)
            .with_context(|| format!("failed to read source of {full_path:?}"))?;
        let code = original.replace("\r\n", "\n");
        Ok(Some(SourceFile {
            package: Arc::clone(package),
            path: path.to_owned(),
            original: Arc::new(original),
            code: Arc::new(code),
            is_top,
        }))
    }

    fn slashes(&self) -> String {
        crate::path::to_slash_path(&self.path)
    }

    fn format_source_location(&self, at: crate::span::LineColumn) -> String {
        format!("{}:{}:{}", self.slashes(), at.line, at.column)
    }
}

/// Discover all mutants in the selected packages, numbering them in order.
pub fn walk_tree(
    workspace_dir: &Utf8Path,
    packages: &[Arc<Package>],
    options: &Options,
) -> Result<Vec<Mutant>> {
    let mut mutants = Vec::new();
    let error_exprs = options.parsed_error_exprs()?;
    for package in packages {
        mutants.append(&mut walk_package(
            workspace_dir,
            package,
            &error_exprs,
            options,
        )?);
    }
    for (id, m) in mutants.iter_mut().enumerate() {
        m.id = id;
    }
    Ok(mutants)
}

/// Walk one package, starting from its top files, discovering files and mutants.
fn walk_package(
    workspace_dir: &Utf8Path,
    package: &Arc<Package>,
    error_exprs: &[Expr],
    options: &Options,
) -> Result<Vec<Mutant>> {
    let mut mutants = Vec::new();
    let mut filename_queue: VecDeque<(Utf8PathBuf, bool)> = package
        .top_sources
        .iter()
        .map(|p| (p.clone(), true))
        .collect();
    while let Some((path, package_top)) = filename_queue.pop_front() {
        let Some(source_file) = SourceFile::load(workspace_dir, &path, package, package_top)?
        else {
            continue;
        };
        let (mut file_mutants, external_mods) = walk_file(&source_file, error_exprs, options)?;
        // Files that don't match the globs are still walked, to find the modules
        // below them, but they contribute no mutants.
        for mod_namespace in &external_mods {
            if let Some(mod_path) = find_mod_source(workspace_dir, &source_file, mod_namespace) {
                filename_queue.push_back((mod_path, false));
            }
        }
        if !options.allows_source_file_path(&source_file.path) {
            continue;
        }
        mutants.append(&mut file_mutants);
    }
    Ok(mutants)
}

/// Find all possible mutants in a source file.
///
/// Returns the mutants found, and the modules referenced by `mod` statements
/// that should be visited later.
pub fn walk_file(
    source_file: &SourceFile,
    error_exprs: &[Expr],
    options: &Options,
) -> Result<(Vec<Mutant>, Vec<ExternalModRef>)> {
    let syn_file = syn::parse_str::<syn::File>(source_file.code.as_str())
        .with_context(|| format!("failed to parse {}", source_file.slashes()))?;
    let mut visitor = DiscoveryVisitor {
        error_exprs,
        external_mods: Vec::new(),
        mutants: Vec::new(),
        mod_namespace_stack: Vec::new(),
        namespace_stack: Vec::new(),
        fn_stack: Vec::new(),
        in_const: false,
        index: LineIndex::new(&source_file.original),
        source_file: source_file.clone(),
        options,
    };
    visitor.visit_file(&syn_file);
    Ok((visitor.mutants, visitor.external_mods))
}

/// Reference to an external module from a source file: the namespace components
/// like `["foo", "bar"]` for `foo::bar`, each possibly with a `#[path="..."]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalModRef {
    parts: Vec<ModNamespace>,
}

/// Namespace for a module defined in a `mod foo { ... }` block or `mod foo;` statement.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ModNamespace {
    /// Identifier of the module (e.g. `foo` for `mod foo;`)
    name: String,
    /// File location override for the module, if specified by `#[path="file"]`
    path_attribute: Option<Utf8PathBuf>,
    /// Location of the module definition in the source file
    source_location: Span,
}

impl ModNamespace {
    /// Returns the name of the module for filesystem purposes
    fn get_filesystem_name(&self) -> &Utf8Path {
        self.path_attribute
            .as_ref()
            .map_or(Utf8Path::new(&self.name), Utf8PathBuf::as_path)
    }
}

/// `syn` visitor that recursively traverses the syntax tree, accumulating places
/// that could be mutated.
struct DiscoveryVisitor<'o> {
    mutants: Vec<Mutant>,
    source_file: SourceFile,
    /// Maps line/column to byte offsets of the file as it is on disk.
    index: LineIndex<'o>,
    mod_namespace_stack: Vec<ModNamespace>,
    namespace_stack: Vec<String>,
    /// The functions we're inside.
    fn_stack: Vec<Arc<Function>>,
    /// True while inside a const context.
    in_const: bool,
    external_mods: Vec<ExternalModRef>,
    error_exprs: &'o [Expr],
    options: &'o Options,
}

impl DiscoveryVisitor<'_> {
    fn enter_function(
        &mut self,
        function_name: &Ident,
        return_type: &ReturnType,
        span: proc_macro2::Span,
    ) -> Arc<Function> {
        self.namespace_stack.push(function_name.to_string());
        let full_function_name = self.namespace_stack.join("::");
        let function = Arc::new(Function {
            function_name: full_function_name,
            return_type: return_type.to_pretty_string(),
            span: span.into(),
        });
        self.fn_stack.push(Arc::clone(&function));
        function
    }

    fn leave_function(&mut self, function: Arc<Function>) {
        self.namespace_stack
            .pop()
            .expect("Namespace stack should not be empty");
        assert_eq!(
            self.fn_stack.pop(),
            Some(function),
            "Function stack mismatch"
        );
    }

    /// Run `f` with the const-context flag set to `value`.
    fn with_const<T>(&mut self, value: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.in_const, value);
        let r = f(self);
        self.in_const = saved;
        r
    }

    fn context(&self, no_if_wrap: bool) -> Context {
        if self.in_const {
            Context::Const
        } else if no_if_wrap {
            Context::NoIfWrap
        } else {
            Context::Runtime
        }
    }

    /// Record a mutant, if the options allow it.
    ///
    /// `edit` is the replaced region, `target` the syntactic unit around it.
    #[allow(clippy::too_many_arguments)]
    fn push_mutant(
        &mut self,
        edit: Span,
        target: Span,
        short_replaced: Option<String>,
        replacement: String,
        genre: Genre,
        mutation_target: Option<MutationTarget>,
        no_if_wrap: bool,
    ) {
        let mut mutant = Mutant {
            id: 0,
            name: String::new(),
            package: self.source_file.package.name.clone(),
            file: self.source_file.path.clone(),
            function: self.fn_stack.last().cloned(),
            span: edit,
            short_replaced,
            replacement,
            genre,
            target: mutation_target,
            edit_range: self.index.range(edit),
            target_range: self.index.range(target),
            context: self.context(no_if_wrap),
            code: Arc::clone(&self.source_file.code),
        };
        mutant.name = mutant.name(true);
        if self.options.allows_mutant(&mutant) {
            self.mutants.push(mutant);
        }
    }

    /// Record that we generated some mutants.
    fn collect_mutant(
        &mut self,
        edit: Span,
        target: Span,
        short_replaced: Option<String>,
        replacement: &TokenStream,
        genre: Genre,
        no_if_wrap: bool,
    ) {
        self.push_mutant(
            edit,
            target,
            short_replaced,
            replacement.to_pretty_string(),
            genre,
            None,
            no_if_wrap,
        );
    }

    fn collect_fn_mutants(&mut self, sig: &Signature, block: &Block) {
        if self.fn_stack.is_empty() {
            return;
        }
        let body_span = function_body_span(block).expect("Empty function body");
        let block_span: Span = block.span().into();
        let repls = return_type_replacements(&sig.output, self.error_exprs);
        let orig_block = block.to_token_stream().to_pretty_string();
        for rep in repls {
            // Comparing strings is a kludge for proc_macro2 not exposing a way
            // to compare token streams. The original block has braces and the
            // replacements don't, so put them back for the comparison.
            let new_block = quote!( { #rep } ).to_token_stream().to_pretty_string();
            if orig_block != new_block {
                self.collect_mutant(body_span, block_span, None, &rep, Genre::FnValue, false);
            }
        }
    }

    /// Call a function with a namespace pushed onto the stack.
    fn in_namespace<F, T>(&mut self, name: &str, f: F) -> T
    where
        F: FnOnce(&mut Self) -> T,
    {
        self.namespace_stack.push(name.to_owned());
        let r = f(self);
        assert_eq!(self.namespace_stack.pop().unwrap(), name);
        r
    }
}

/// True if a chain of `&&` contains a `let`, which makes it a let chain.
fn is_let_chain(i: &syn::ExprBinary) -> bool {
    fn has_let(e: &Expr) -> bool {
        match e {
            Expr::Let(_) => true,
            Expr::Binary(b) if matches!(b.op, BinOp::And(_)) => {
                has_let(&b.left) || has_let(&b.right)
            }
            _ => false,
        }
    }
    matches!(i.op, BinOp::And(_)) && (has_let(&i.left) || has_let(&i.right))
}

impl<'ast> Visit<'ast> for DiscoveryVisitor<'_> {
    fn visit_expr_call(&mut self, i: &'ast syn::ExprCall) {
        if attrs_excluded(&i.attrs) {
            return;
        }
        if let Expr::Path(ExprPath { path, .. }) = &*i.func
            && self
                .options
                .skip_calls
                .iter()
                .any(|s| path_ends_with(path, s))
        {
            return;
        }
        syn::visit::visit_expr_call(self, i);
    }

    fn visit_expr_method_call(&mut self, i: &'ast syn::ExprMethodCall) {
        if attrs_excluded(&i.attrs) {
            return;
        }
        if self.options.skip_calls.iter().any(|s| i.method == s) {
            return;
        }
        syn::visit::visit_expr_method_call(self, i);
    }

    /// Visit a source file.
    fn visit_file(&mut self, i: &'ast File) {
        if attrs_excluded(&i.attrs) {
            return;
        }
        syn::visit::visit_file(self, i);
    }

    /// Visit top-level `fn foo()`.
    fn visit_item_fn(&mut self, i: &'ast ItemFn) {
        if fn_sig_excluded(&i.sig) || attrs_excluded(&i.attrs) || block_is_empty(&i.block) {
            return;
        }
        let function = self.enter_function(&i.sig.ident, &i.sig.output, i.span());
        self.with_const(i.sig.constness.is_some(), |v| {
            v.collect_fn_mutants(&i.sig, &i.block);
            syn::visit::visit_item_fn(v, i);
        });
        self.leave_function(function);
    }

    /// Visit `fn foo()` within an `impl`.
    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        // Don't look inside constructors (called "new") because there's often no good
        // alternative.
        if fn_sig_excluded(&i.sig)
            || attrs_excluded(&i.attrs)
            || i.sig.ident == "new"
            || block_is_empty(&i.block)
        {
            return;
        }
        let function = self.enter_function(&i.sig.ident, &i.sig.output, i.span());
        self.with_const(i.sig.constness.is_some(), |v| {
            v.collect_fn_mutants(&i.sig, &i.block);
            syn::visit::visit_impl_item_fn(v, i);
        });
        self.leave_function(function);
    }

    /// Visit `fn foo() { ... }` within a trait, i.e. a default implementation of a function.
    fn visit_trait_item_fn(&mut self, i: &'ast syn::TraitItemFn) {
        if fn_sig_excluded(&i.sig) || attrs_excluded(&i.attrs) || i.sig.ident == "new" {
            return;
        }
        if let Some(block) = &i.default {
            if block_is_empty(block) {
                return;
            }
            let function = self.enter_function(&i.sig.ident, &i.sig.output, i.span());
            self.with_const(i.sig.constness.is_some(), |v| {
                v.collect_fn_mutants(&i.sig, block);
                syn::visit::visit_trait_item_fn(v, i);
            });
            self.leave_function(function);
        }
    }

    /// Visit `impl Foo { ...}` or `impl Debug for Foo { ... }`.
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if attrs_excluded(&i.attrs) {
            return;
        }
        let type_name = i.self_ty.to_pretty_string();
        let name = if let Some((_, trait_path, _)) = &i.trait_ {
            if path_ends_with(trait_path, "Default") {
                // Can't think of how to generate a viable different default.
                return;
            }
            format!(
                "<impl {trait} for {type_name}>",
                trait = trait_path.to_pretty_string()
            )
        } else {
            type_name
        };
        self.in_namespace(&name, |v| syn::visit::visit_item_impl(v, i));
    }

    /// Visit `trait Foo { ... }`
    fn visit_item_trait(&mut self, i: &'ast syn::ItemTrait) {
        let name = i.ident.to_pretty_string();
        if attrs_excluded(&i.attrs) {
            return;
        }
        self.in_namespace(&name, |v| syn::visit::visit_item_trait(v, i));
    }

    /// Visit `mod foo { ... }` or `mod foo;`.
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let mod_name = node.ident.unraw().to_string();
        if attrs_excluded(&node.attrs) {
            return;
        }

        let source_location = Span::from(node.span());

        // Extract path attribute value, if any (e.g. `#[path="..."]`)
        let path_attribute = match find_path_attribute(&node.attrs) {
            Ok(path) => path,
            Err(path_attribute) => {
                let definition_site = self
                    .source_file
                    .format_source_location(source_location.start);
                eprintln!(
                    "error: invalid filesystem traversal in mod path attribute {path_attribute:?} \
                     at {definition_site} (mod {mod_name})"
                );
                return;
            }
        };
        let mod_namespace = ModNamespace {
            name: mod_name,
            path_attribute,
            source_location,
        };
        self.mod_namespace_stack.push(mod_namespace.clone());

        // If there's no content in braces, then this is a `mod foo;`
        // statement referring to an external file.
        if node.content.is_none() {
            self.external_mods.push(ExternalModRef {
                parts: self.mod_namespace_stack.clone(),
            });
        }
        self.in_namespace(&mod_namespace.name, |v| syn::visit::visit_item_mod(v, node));
        assert_eq!(self.mod_namespace_stack.pop(), Some(mod_namespace));
    }

    /// Visit `a op b` expressions.
    fn visit_expr_binary(&mut self, i: &'ast syn::ExprBinary) {
        if attrs_excluded(&i.attrs) {
            return;
        }
        let replacements = match i.op {
            // We don't generate `<=` from `==` because it can too easily go
            // wrong with unsigned types compared to 0.
            BinOp::Eq(_) => vec![quote! { != }],
            BinOp::Ne(_) => vec![quote! { == }],
            BinOp::And(_) => vec![quote! { || }],
            BinOp::Or(_) => vec![quote! { && }],
            BinOp::Lt(_) => vec![quote! { == }, quote! {>}, quote! { <= }],
            BinOp::Gt(_) => vec![quote! { == }, quote! {<}, quote! { >= }],
            BinOp::Le(_) => vec![quote! {>}],
            BinOp::Ge(_) => vec![quote! {<}],
            BinOp::Add(_) => vec![quote! {-}, quote! {*}],
            BinOp::AddAssign(_) => vec![quote! {-=}, quote! {*=}],
            BinOp::Sub(_) | BinOp::Mul(_) => vec![quote! {+}, quote! {/}],
            BinOp::SubAssign(_) | BinOp::MulAssign(_) => vec![quote! {+=}, quote! {/=}],
            BinOp::Div(_) => vec![quote! {%}, quote! {*}],
            BinOp::DivAssign(_) => vec![quote! {%=}, quote! {*=}],
            BinOp::Rem(_) => vec![quote! {/}, quote! {+}],
            BinOp::RemAssign(_) => vec![quote! {/=}, quote! {+=}],
            BinOp::Shl(_) => vec![quote! {>>}],
            BinOp::ShlAssign(_) => vec![quote! {>>=}],
            BinOp::Shr(_) => vec![quote! {<<}],
            BinOp::ShrAssign(_) => vec![quote! {<<=}],
            BinOp::BitAnd(_) => vec![quote! {|}, quote! {^}],
            BinOp::BitAndAssign(_) => vec![quote! {|=}],
            BinOp::BitOr(_) => vec![quote! {&}, quote! {^}],
            BinOp::BitOrAssign(_) => vec![quote! {&=}],
            BinOp::BitXor(_) => vec![quote! {|}, quote! {&}],
            BinOp::BitXorAssign(_) => vec![quote! {|=}, quote! {&=}],
            _ => Vec::new(),
        };
        let no_if_wrap = is_let_chain(i);
        let target: Span = i.span().into();
        for rep in replacements {
            self.collect_mutant(
                i.op.span().into(),
                target,
                None,
                &rep,
                Genre::BinaryOperator,
                no_if_wrap,
            );
        }
        syn::visit::visit_expr_binary(self, i);
    }

    fn visit_expr_unary(&mut self, i: &'ast syn::ExprUnary) {
        if attrs_excluded(&i.attrs) {
            return;
        }
        if matches!(i.op, UnOp::Not(_) | UnOp::Neg(_)) {
            self.collect_mutant(
                i.op.span().into(),
                i.span().into(),
                None,
                &quote! {},
                Genre::UnaryOperator,
                false,
            );
        }
        syn::visit::visit_expr_unary(self, i);
    }

    fn visit_expr_match(&mut self, i: &'ast syn::ExprMatch) {
        // While it's not currently possible to annotate expressions with custom attributes, this
        // limitation could be lifted in the future.
        if attrs_excluded(&i.attrs) {
            return;
        }

        let has_catchall = i
            .arms
            .iter()
            .any(|arm| matches!(arm.pat, syn::Pat::Wild(_)));
        if has_catchall {
            // Successively delete each of the match arms, allowing them to hit the catch-all arm,
            // which will often be a viable mutant.
            for arm in &i.arms {
                if matches!(arm.pat, syn::Pat::Wild(_)) || arm.guard.is_some() {
                    // Don't delete the `_ => whatever` arm, because that will very likely fail to compile.
                    //
                    // Also, don't delete arms with guard expressions, because the replacement of
                    // the guard with 'false' below is logically equivalent to removing the arm.
                    continue;
                }
                let arm_span: Span = arm.span().into();
                self.collect_mutant(
                    arm_span,
                    arm_span,
                    Some(arm.pat.to_pretty_string()),
                    &quote! {},
                    Genre::MatchArm,
                    true,
                );
            }
        }

        i.arms
            .iter()
            .flat_map(|arm| &arm.guard)
            .for_each(|(_if, guard_expr)| {
                let guard_span: Span = guard_expr.span().into();
                self.collect_mutant(
                    guard_span,
                    guard_span,
                    None,
                    &quote! { true },
                    Genre::MatchArmGuard,
                    false,
                );
                self.collect_mutant(
                    guard_span,
                    guard_span,
                    None,
                    &quote! { false },
                    Genre::MatchArmGuard,
                    false,
                );
            });

        syn::visit::visit_expr_match(self, i);
    }

    fn visit_expr_struct(&mut self, i: &'ast syn::ExprStruct) {
        if attrs_excluded(&i.attrs) {
            return;
        }

        // Check if this struct has a base (default) expression like `..Default::default()`
        if i.rest.is_some() {
            let struct_name = i.path.to_pretty_string();

            // Generate a mutant for each field by deleting it, including its
            // trailing comma in the span.
            for pair in i.fields.pairs() {
                let field = pair.value();
                if let syn::Member::Named(field_name) = &field.member {
                    let span = if let Some(comma) = pair.punct() {
                        Span {
                            start: field.span().start().into(),
                            end: comma.span().end().into(),
                        }
                    } else {
                        field.span().into()
                    };
                    self.push_mutant(
                        span,
                        span,
                        None,
                        String::new(),
                        Genre::StructField,
                        Some(MutationTarget::StructLiteralField {
                            field_name: field_name.to_string(),
                            struct_name: struct_name.clone(),
                        }),
                        true,
                    );
                }
            }
        }

        syn::visit::visit_expr_struct(self, i);
    }

    // Const contexts: expressions here are evaluated at compile time.

    fn visit_item_const(&mut self, i: &'ast syn::ItemConst) {
        self.with_const(true, |v| syn::visit::visit_item_const(v, i));
    }

    fn visit_item_static(&mut self, i: &'ast syn::ItemStatic) {
        self.with_const(true, |v| syn::visit::visit_item_static(v, i));
    }

    fn visit_impl_item_const(&mut self, i: &'ast syn::ImplItemConst) {
        self.with_const(true, |v| syn::visit::visit_impl_item_const(v, i));
    }

    fn visit_trait_item_const(&mut self, i: &'ast syn::TraitItemConst) {
        self.with_const(true, |v| syn::visit::visit_trait_item_const(v, i));
    }

    fn visit_expr_const(&mut self, i: &'ast syn::ExprConst) {
        self.with_const(true, |v| syn::visit::visit_expr_const(v, i));
    }

    fn visit_type_array(&mut self, i: &'ast syn::TypeArray) {
        self.visit_type(&i.elem);
        self.with_const(true, |v| v.visit_expr(&i.len));
    }

    fn visit_expr_repeat(&mut self, i: &'ast syn::ExprRepeat) {
        for attr in &i.attrs {
            self.visit_attribute(attr);
        }
        self.visit_expr(&i.expr);
        self.with_const(true, |v| v.visit_expr(&i.len));
    }

    fn visit_generic_argument(&mut self, i: &'ast syn::GenericArgument) {
        if let syn::GenericArgument::Const(e) = i {
            self.with_const(true, |v| v.visit_expr(e));
        } else {
            syn::visit::visit_generic_argument(self, i);
        }
    }

    fn visit_const_param(&mut self, i: &'ast syn::ConstParam) {
        for attr in &i.attrs {
            self.visit_attribute(attr);
        }
        self.visit_type(&i.ty);
        if let Some(default) = &i.default {
            self.with_const(true, |v| v.visit_expr(default));
        }
    }

    fn visit_variant(&mut self, i: &'ast syn::Variant) {
        for attr in &i.attrs {
            self.visit_attribute(attr);
        }
        self.visit_fields(&i.fields);
        if let Some((_, discriminant)) = &i.discriminant {
            self.with_const(true, |v| v.visit_expr(discriminant));
        }
    }
}

// Get the span of the block excluding the braces, or None if it is empty.
fn function_body_span(block: &Block) -> Option<Span> {
    Some(Span {
        start: block.stmts.first()?.span().start().into(),
        end: block.stmts.last()?.span().end().into(),
    })
}

/// Find a new source file referenced by a `mod` statement.
///
/// Possibly, our heuristics just won't be able to find which file it is,
/// in which case we return `Ok(None)`.
fn find_mod_source(
    tree_root: &Utf8Path,
    parent: &SourceFile,
    mod_namespace: &ExternalModRef,
) -> Option<Utf8PathBuf> {
    // First, work out whether the mod will be a sibling in the same directory, or
    // in a child directory.
    //
    // 1. The parent is "src/foo.rs" and `mod bar` means "src/foo/bar.rs".
    //
    // 2. The parent is "src/lib.rs" (a target top file) and `mod bar` means "src/bar.rs".
    //
    // 3. The parent is "src/foo/mod.rs" and so `mod bar` means "src/foo/bar.rs".
    //
    // 4. A path attribute on a mod statement when there is no enclosing mod block
    //     E.g. for parent file "src/a/parent_file.rs",
    //     ```
    //     // `path` is relative to the directory where the source file is located
    //     #[path="foo_file.rs"] // resolves to: src/a/foo_file.rs
    //     mod foo;
    //
    //     mod bar {
    //         // `path` is relative to the directory of the enclosing module block
    //         #[path="baz_file.rs"] // resolves to: src/a/parent_file/bar/baz_file.rs
    //         mod baz;
    //     }
    //     ```
    //
    // Having determined the right directory then we can follow the path attribute, or
    // if no path is specified, then look for either `foo.rs` or `foo/mod.rs`.

    let (mod_child, mod_parents) = mod_namespace
        .parts
        .split_last()
        .expect("mod namespace is empty");

    // TODO: Beyond #115, we should probably remove all special handling of
    // `mod.rs` here by remembering how we found this file, and whether it
    // is above or inside the directory corresponding to its module?

    let parent_path = &parent.path;
    let mut search_dir = if parent.is_top
        || parent_path.ends_with("mod.rs")
        // NOTE: Path attribute on a top-level `mod foo;` (no enclosing block)
        //       ignores the parent module path
        || (mod_child.path_attribute.is_some() && mod_parents.is_empty())
    {
        parent_path
            .parent()
            .expect("mod path has no parent")
            .to_owned() // src/lib.rs -> src/
    } else {
        parent_path.with_extension("") // foo.rs -> foo/
    };

    search_dir.extend(mod_parents.iter().map(ModNamespace::get_filesystem_name));

    let mod_child_candidates = if let Some(filesystem_name) = &mod_child.path_attribute {
        vec![search_dir.join(filesystem_name)]
    } else {
        [".rs", "/mod.rs"]
            .iter()
            .map(|tail| search_dir.join(mod_child.name.clone() + tail))
            .collect()
    };

    let mut tried_paths = Vec::new();
    for relative_path in mod_child_candidates {
        let full_path = tree_root.join(&relative_path);
        if full_path.is_file() {
            return Some(relative_path);
        }
        tried_paths.push(full_path);
    }
    let mod_name = &mod_child.name;
    let definition_site = parent.format_source_location(mod_child.source_location.start);
    eprintln!(
        "warning: referent of mod {mod_name} not found (defined at {definition_site}; tried {tried_paths:?})"
    );
    None
}

/// True if the signature of a function is such that it should be excluded.
fn fn_sig_excluded(sig: &syn::Signature) -> bool {
    sig.unsafety.is_some()
}

/// True if any of the attrs indicate that we should skip this node and everything inside it.
///
/// This checks for `#[cfg(test)]`, attributes whose path ends with `test` (like `#[test]` or `#[tokio::test]`), and `#[mutants::skip]`.
fn attrs_excluded(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|attr| attr_is_cfg_test(attr) || attr_is_test(attr) || attr_is_mutants_skip(attr))
}

/// True if the block (e.g. the contents of a function) is empty.
fn block_is_empty(block: &syn::Block) -> bool {
    block.stmts.is_empty()
}

/// True if the attribute looks like `#[cfg(test)]`, or has "test"
/// anywhere in it.
fn attr_is_cfg_test(attr: &Attribute) -> bool {
    if !path_is(attr.path(), &["cfg"]) {
        return false;
    }
    let mut contains_test = false;
    if attr
        .parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                contains_test = true;
            }
            Ok(())
        })
        .is_err()
    {
        return false;
    }
    contains_test
}

/// True if the attribute path ends with `test`.
///
/// This matches `#[test]`, `#[tokio::test]`, `#[sqlx::test]`, etc.
fn attr_is_test(attr: &Attribute) -> bool {
    path_ends_with(attr.path(), "test")
}

fn path_is(path: &syn::Path, idents: &[&str]) -> bool {
    path.segments.iter().map(|ps| &ps.ident).eq(idents.iter())
}

/// True if the path ends with this identifier.
///
/// This is used as a heuristic to match types without being sensitive to which
/// module they are in, or to match functions without being sensitive to which
/// type they might be associated with.
///
/// This does not check type arguments.
fn path_ends_with(path: &syn::Path, ident: &str) -> bool {
    path.segments.last().is_some_and(|s| s.ident == ident)
}

/// True if the attribute contains `mutants::skip`.
///
/// This for example returns true for `#[mutants::skip]` or `#[cfg_attr(test, mutants::skip)]`.
fn attr_is_mutants_skip(attr: &Attribute) -> bool {
    if path_is(attr.path(), &["mutants", "skip"]) {
        return true;
    }
    if !path_is(attr.path(), &["cfg_attr"]) {
        return false;
    }
    let mut skip = false;
    if attr
        .parse_nested_meta(|meta| {
            if path_is(&meta.path, &["mutants", "skip"]) {
                skip = true;
            }
            Ok(())
        })
        .is_err()
    {
        return false;
    }
    skip
}

/// Finds the first path attribute (`#[path = "..."]`)
///
/// # Errors
/// Returns an error if the path attribute contains a dubious path (leading `/`)
fn find_path_attribute(attrs: &[Attribute]) -> std::result::Result<Option<Utf8PathBuf>, String> {
    attrs
        .iter()
        .find_map(|attr| match &attr.meta {
            syn::Meta::NameValue(meta) if meta.path.is_ident("path") => {
                let syn::Expr::Lit(expr_lit) = &meta.value else {
                    return None;
                };
                let syn::Lit::Str(lit_str) = &expr_lit.lit else {
                    return None;
                };
                let path = lit_str.value();

                // refuse to follow absolute paths
                if path.starts_with('/') {
                    Some(Err(path))
                } else {
                    Some(Ok(Utf8PathBuf::from(path)))
                }
            }
            _ => None,
        })
        .transpose()
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::options::{Config, Filters};

    fn source(code: &str) -> SourceFile {
        let package = Arc::new(Package {
            name: "t".into(),
            relative_dir: Utf8PathBuf::new(),
            top_sources: vec!["src/lib.rs".into()],
            all_sources: vec!["src/lib.rs".into()],
        });
        SourceFile {
            package,
            path: "src/lib.rs".into(),
            original: Arc::new(code.to_owned()),
            code: Arc::new(code.replace("\r\n", "\n")),
            is_top: true,
        }
    }

    fn mutate(code: &str) -> Vec<Mutant> {
        let options = Options::new(&Filters::default(), &Config::default()).unwrap();
        walk_file(&source(code), &[], &options).unwrap().0
    }

    /// The text of the file at a byte range, from the code that was mutated.
    fn text<'a>(code: &'a str, range: &std::ops::Range<usize>) -> &'a str {
        &code[range.clone()]
    }

    fn find<'a>(mutants: &'a [Mutant], name_part: &str) -> &'a Mutant {
        let hits: Vec<_> = mutants
            .iter()
            .filter(|m| m.name(false).contains(name_part))
            .collect();
        assert_eq!(hits.len(), 1, "{name_part}: {hits:?}");
        hits[0]
    }

    #[test]
    fn ranges_of_each_genre() {
        let code = "fn f(a: u32, b: u32, c: Option<u8>) -> bool {\n    let x = !(a + b > 3);\n    match c {\n        Some(1) => 1,\n        Some(n) if n == 2 => 2,\n        _ => 3,\n    };\n    x\n}\nfn g() -> S {\n    S { a: 1, b: 2, ..Default::default() }\n}\n";
        let ms = mutate(code);

        let m = find(&ms, "replace f -> bool with true");
        assert_eq!(m.genre, Genre::FnValue);
        assert_eq!(
            text(code, &m.edit_range).lines().next(),
            Some("let x = !(a + b > 3);")
        );
        assert!(text(code, &m.target_range).starts_with("{\n    let x"));
        assert!(text(code, &m.target_range).ends_with("x\n}"));

        let m = find(&ms, "replace + with - in f");
        assert_eq!(text(code, &m.edit_range), "+");
        assert_eq!(text(code, &m.target_range), "a + b");

        let m = find(&ms, "replace > with < in f");
        assert_eq!(text(code, &m.edit_range), ">");
        assert_eq!(text(code, &m.target_range), "a + b > 3");

        let m = find(&ms, "delete ! in f");
        assert_eq!(text(code, &m.edit_range), "!");
        assert_eq!(text(code, &m.target_range), "!(a + b > 3)");

        let m = find(&ms, "delete match arm Some(1)");
        assert_eq!(text(code, &m.edit_range), "Some(1) => 1,");
        assert_eq!(m.target_range, m.edit_range);

        assert_eq!(m.context, Context::NoIfWrap);
        let ms_true = find(&ms, "replace match guard n == 2 with true");
        assert_eq!(text(code, &ms_true.edit_range), "n == 2");
        assert_eq!(ms_true.target_range, ms_true.edit_range);

        let m = find(&ms, "delete field a from struct S expression");
        assert_eq!(text(code, &m.edit_range), "a: 1,");
        let m = find(&ms, "delete field b from struct S expression");
        assert_eq!(text(code, &m.edit_range), "b: 2,");
    }

    #[test]
    fn byte_ranges_index_the_file_on_disk_with_crlf_and_non_ascii() {
        let code = "fn f(a: u32) -> u32 {\r\n    let s = \"é→é\";\r\n    let n = s.len() as u32 + a;\r\n    n\r\n}\r\n";
        let ms = mutate(code);
        let m = find(&ms, "replace + with - in f");
        assert_eq!(text(code, &m.edit_range), "+");
        assert_eq!(text(code, &m.target_range), "s.len() as u32 + a");
        assert_eq!((m.span.start.line, m.span.start.column), (3, 28));
        let m = find(&ms, "replace f -> u32 with 0");
        assert_eq!(
            text(code, &m.edit_range),
            "let s = \"é→é\";\r\n    let n = s.len() as u32 + a;\r\n    n"
        );
    }

    fn context_of(code: &str, name_part: &str) -> Context {
        find(&mutate(code), name_part).context
    }

    #[test]
    fn const_contexts_are_classified() {
        use Context::{Const, Runtime};
        let cases: &[(&str, &str, Context)] = &[
            (
                "const fn f(a: u32) -> u32 { a + 1 }",
                "replace + with - in f",
                Const,
            ),
            (
                "const fn f(a: u32) -> u32 { a + 1 }",
                "replace f -> u32 with 0",
                Const,
            ),
            ("const A: u32 = 1 + 2;", "replace + with -", Const),
            ("static A: u32 = 1 + 2;", "replace + with -", Const),
            (
                "struct S;\nimpl S { const A: u32 = 1 + 2; }",
                "replace + with -",
                Const,
            ),
            (
                "trait T { const A: u32 = 1 + 2; }",
                "replace + with -",
                Const,
            ),
            ("struct S { a: [u8; 1 + 2] }", "replace + with -", Const),
            (
                "fn f() { let a: [u8; 1 + 2] = [0; 3]; }",
                "replace + with - in f",
                Const,
            ),
            (
                "fn f() { let a = [0; 1 + 2]; }",
                "replace + with - in f",
                Const,
            ),
            (
                "fn f() { let a = [1 + 2; 3]; }",
                "replace + with - in f",
                Runtime,
            ),
            (
                "fn f() -> G<{ 1 + 2 }> { G }",
                "replace + with - in f",
                Const,
            ),
            ("enum E { A = 1 << 2 }", "replace << with >>", Const),
            ("enum E { A = 1 << 2 }", "replace << with >>", Const),
            (
                "fn f() { let a = const { 1 + 2 }; }",
                "replace + with - in f",
                Const,
            ),
            (
                "fn f(a: u32) -> u32 { a + 1 }",
                "replace + with - in f",
                Runtime,
            ),
            (
                "const fn f(a: u32) -> u32 { fn g(b: u32) -> u32 { b + 1 } g(a) }",
                "replace + with - in f::g",
                Runtime,
            ),
            (
                "const A: u32 = { fn g(b: u32) -> u32 { b + 1 } g(2) };",
                "replace + with - in g",
                Runtime,
            ),
        ];
        for (code, name, want) in cases {
            assert_eq!(context_of(code, name), *want, "{name} in {code}");
        }
    }

    #[test]
    fn let_chains_cannot_be_wrapped_in_if() {
        let code = "fn f(a: Option<u32>, b: bool, c: bool) -> bool {\n    if let Some(x) = a && x > 1 && b { b && c } else { b || c }\n}\n";
        let ms = mutate(code);
        let chain: Vec<_> = ms
            .iter()
            .filter(|m| m.name(false).contains("replace && with || in f"))
            .collect();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].context, Context::NoIfWrap);
        assert_eq!(chain[1].context, Context::NoIfWrap);
        assert_eq!(chain[2].context, Context::Runtime);
        assert_eq!(find(&ms, "replace > with < in f").context, Context::Runtime);
        assert_eq!(
            find(&ms, "replace || with && in f").context,
            Context::Runtime
        );
    }

    #[test]
    fn whole_kitchen_sink_is_consistent() {
        let root =
            Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitchen_sink");
        let package = Arc::new(Package {
            name: "kitchen-sink".into(),
            relative_dir: Utf8PathBuf::new(),
            top_sources: vec!["src/lib.rs".into(), "src/bin/tool.rs".into()],
            all_sources: vec!["src/lib.rs".into(), "src/bin/tool.rs".into()],
        });
        let options = Options::new(&Filters::default(), &Config::default()).unwrap();
        let mutants = walk_tree(&root, &[package], &options).unwrap();
        assert!(mutants.len() > 300);
        for (i, m) in mutants.iter().enumerate() {
            assert_eq!(m.id, i);
            let disk = std::fs::read_to_string(root.join(&m.file)).unwrap();
            assert!(
                m.target_range.start <= m.edit_range.start
                    && m.edit_range.end <= m.target_range.end
            );
            assert_eq!(
                text(&disk, &m.edit_range).replace("\r\n", "\n"),
                m.original_text(),
                "{}",
                m.name
            );
        }
        let ctx = |c| mutants.iter().filter(|m| m.context == c).count();
        assert!(ctx(Context::Const) > 10);
        assert!(ctx(Context::NoIfWrap) >= 2);
        assert!(ctx(Context::Runtime) > 200);
        assert!(mutants.iter().any(|m| m.file.as_str() == "src/crlf.rs"));
    }
}

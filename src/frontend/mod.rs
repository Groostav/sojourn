//! Source text to [`Ast`].
//!
//! Everything here is meaning-preserving. [`parse`] lexes, parses and lowers to
//! [`crate::ast`] ([`syntax::raw_ast`]), then [`rewrite::canonicalize`] normalises the tree
//! *without changing what it computes* — folding constants, inverting monotone
//! comparisons, unrolling aggregates over literal bounds, collecting a term
//! multiplied by itself into a power.
//!
//! That is the line this module draws. A pass that makes the tree easier to
//! analyse belongs here; a pass that lowers it toward one consumer's needs
//! belongs to that consumer. `src/README.md` has the table of where each pass
//! falls and why the order between them is forced.

use crate::ast;
use crate::diagnostics::{CompilationFailure, Fault, Problem, ProblemKind, Span};

pub(crate) mod generated;
pub(crate) mod rewrite;
pub(crate) mod syntax;

pub(crate) use syntax::parses_as_variable;

/// Compiles source text into an evaluable expression.
///
/// # Errors
/// Returns [`CompilationFailure`] with every problem found; compilation does
/// not stop at the first one.
pub(crate) fn parse(source: &str) -> Result<Ast, CompilationFailure> {
    if source.is_empty() {
        return Err(CompilationFailure {
            source: source.to_owned(),
            problems: vec![Problem::new(
                ProblemKind::EmptyExpression,
                source,
                Span::new(0, 0),
            )],
        });
    }

    let raw = match syntax::raw_ast(source) {
        Ok(raw) => raw,
        Err(problems) => {
            return Err(CompilationFailure {
                source: source.to_owned(),
                problems,
            });
        }
    };

    let render = |faults| failure(source, faults);

    // The canonical form: constants folded, monotone comparisons inverted,
    // aggregates over known bounds unrolled, repeated factors collected into
    // powers — in that order, for the reasons `rewrite::canonicalize` gives.
    let program = rewrite::canonicalize(raw.program).map_err(render)?;

    // Read off the finished tree rather than tracked while building it: one
    // definition of the flag, and the same one `resolve_subscripts` recomputes.
    let contains_dynamic_lookup = rewrite::holds_subscript(&program.body);

    Ok(Ast {
        source: source.to_owned(),
        program,
        symbols: raw.symbols,
        contains_dynamic_lookup,
        is_constraint: raw.is_constraint,
    })
}

/// Faults rendered against the source they were found in.
///
/// A rewrite pass reports kind and span and never carries the source; the
/// boundary that has it — this parse, [`eval::bind`](crate::eval::bind)
/// resolving subscripts, [`ConstraintSystem::new`](crate::ConstraintSystem::new)
/// doing the same — turns them into problems here.
pub(crate) fn failure(source: &str, faults: Vec<Fault>) -> CompilationFailure {
    CompilationFailure {
        source: source.to_owned(),
        problems: faults
            .into_iter()
            .map(|fault| Problem::new(fault.kind, source, fault.span))
            .collect(),
    }
}

/// A parsed expression in canonical form, ready to be bound to a
/// [`Schema`](crate::Schema): a [`syntax::RawAst`] that
/// [`rewrite::canonicalize`] has run over. Both backends rely on that form.
///
/// Crate-private: a caller hands source text to `compile`, `compile_system`
/// or `ConstraintSystem::new` and never sees the tree between.
///
/// The fields are private to `frontend`, so only this module and its passes
/// can build or take apart an `Ast`; the backends read it through the
/// accessors and cannot hand one another a tree that skipped canonicalisation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ast {
    source: String,
    program: ast::Program,
    /// Distinct statically-referenced names in first-reference order.
    /// `ast::GlobalId` indexes into *this*, not into the schema — the AST is
    /// built before any schema exists, so binding (`eval::bind`) is what maps
    /// these onto row positions.
    symbols: Vec<String>,
    contains_dynamic_lookup: bool,
    is_constraint: bool,
}

impl Ast {
    /// The source text this was compiled from.
    #[must_use]
    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    /// The canonical tree.
    #[must_use]
    pub(crate) const fn program(&self) -> &ast::Program {
        &self.program
    }

    /// Whether the expression uses `var[i]` dynamic lookup.
    ///
    /// A subscript is a one-based index into the whole [`Schema`](crate::Schema) in
    /// declaration order, so such an expression can read a variable it never
    /// names and its [`symbols`](Ast::symbols) are not the whole story.
    /// **A caller must not prune columns it believes are unreferenced while
    /// this is true.**
    ///
    #[must_use]
    pub(crate) const fn contains_dynamic_lookup(&self) -> bool {
        self.contains_dynamic_lookup
    }

    /// Whether the source was a boolean expression, and therefore whether the
    /// result should be read as a constraint residual rather than a value.
    #[must_use]
    pub(crate) const fn is_constraint(&self) -> bool {
        self.is_constraint
    }

    /// Statically-referenced names in first-reference order, indexed by
    /// `ast::GlobalId`.
    #[must_use]
    pub(crate) fn symbols(&self) -> &[String] {
        &self.symbols
    }

    /// Where each symbol is first referenced, indexed like
    /// [`symbols`](Self::symbols): the span to put a caret under when a
    /// symbol turns out to be unbound. Read off the tree — every `Global`
    /// node carries the span of the reference that made it — rather than
    /// recorded beside `symbols`, so it cannot drift from what the tree says.
    /// `None` for a symbol nothing references, which the translator never
    /// produces but the walk does not assume.
    pub(crate) fn reference_spans(&self) -> Vec<Option<Span>> {
        let mut spans = vec![None; self.symbols.len()];
        for node in self.program.body.iter_preorder() {
            if let ast::Kind::Global(id) = node.kind {
                let slot = &mut spans[id.index()];
                if slot.is_none_or(|held| node.span < held) {
                    *slot = Some(node.span);
                }
            }
        }
        spans
    }
}

//! The evaluator: an `Ast` lowered to a flat tape, run a tile of samples at a
//! time or one row at a time.
//!
//! `tape.rs` is the instruction set, `irgen.rs` produces it, `regalloc.rs`
//! packs its temporaries, `differentiate.rs` turns a tape into the tape of
//! its gradient, and the two executors — `tile.rs` for a batch, `lane.rs`
//! for a row — run either. Every tape is straight-line: the front end
//! unrolls `sum` and `prod` or refuses them, so nothing here loops. It replaced a tree-walking evaluator, and was
//! held to that walker's answers bit for bit over a few thousand random and
//! adversarial rows before the walker was deleted; what remains as the spec is
//! the corpus, the runtime-error tests and `tests/special_values.rs`.

mod differentiate;
pub(crate) mod irgen;
mod lane;
mod regalloc;
mod simd;
pub(crate) mod tape;
mod tile;
pub(crate) mod wgsl;

use std::collections::BTreeSet;

use faer::{Col, Mat, MatRef};

use crate::Ast;
use crate::diagnostics::EvaluationFailure;
use crate::diagnostics::{CompilationFailure, Fault, Problem, ProblemKind, RuntimeProblem, Span};
use crate::frontend::rewrite;

use tape::{AllocatedTape, Register};
use tile::{RegisterFile, TILE};

/// Java's `Double.MIN_NORMAL`, the nudge that makes a *strict* inequality
/// representable when `<= 0` means true.
///
/// It is meant to vanish into rounding at any meaningful magnitude — `(4 - 6) + ε`
/// is exactly `-2.0` — and to survive only when the difference is zero, which is
/// precisely when strict and non-strict differ: `(6 - 6) + ε` is `ε`, which is
/// `> 0`, so `6 > 6` is false.
pub(crate) const EPSILON: f64 = f64::MIN_POSITIVE;

/// Compiles `source` against `variables` with the defaults: the shorthand
/// for [`Compiler::new().compile(..)`](Compiler::compile).
///
/// `variables` is every name the expression may use: a plain list (`&["x1",
/// "x2"]`, a `Vec`, a slice), every one of them an input, or
/// [`Variables::classified`], inputs and intermediates. The rows a batch
/// comes in are [`CompiledExpression::rows`].
///
/// # Errors
/// [`CompilationFailure`] with every problem found — parsing does not stop at
/// the first; a name `variables` lacks is one, at its first reference, and so
/// is a literal subscript past the inputs, at the subscript.
pub fn compile<'a>(
    source: &str,
    variables: impl Into<Variables<'a>>,
) -> Result<CompiledExpression, CompilationFailure> {
    Compiler::new().compile(source, variables)
}

/// The names an expression may use, and which of them `var[i]` may index.
///
/// Two kinds. An **input** is a coordinate of the design vector: known
/// before anything runs, indexed by `var[i]`, one-based, in declaration
/// order, and given a row whether the expression names it or not — so
/// `var[5]` is the same input whatever else the expression reads. An
/// **intermediate** is a name produced elsewhere — a simulation's result,
/// another expression's output — that may not exist yet when the inputs do:
/// bound by name only, out of reach of every subscript, and given a row only
/// where the expression names it, so a caller is never asked for a value the
/// expression does not use.
///
/// A plain list converts into this as every name an input, which is what
/// [`compile`] has always taken; [`classified`](Self::classified) says which
/// is which. A value, not a trait: the conversion happens once at the door
/// and everything past it is one code path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variables<'a> {
    inputs: Vec<&'a str>,
    intermediates: Vec<&'a str>,
}

impl<'a> Variables<'a> {
    /// `inputs` in declaration order, which `var[i]` indexes, and
    /// `intermediates` in the order their rows follow the inputs.
    ///
    /// # Panics
    /// If a name appears twice, in one list or across both. A document that
    /// declares a name twice is its parser's to refuse, under the name the
    /// author wrote; by here it is the caller's bug, and binding the first
    /// silently would make `var[i]` and a row disagree about what it is.
    #[must_use]
    pub fn classified<S: AsRef<str>>(inputs: &'a [S], intermediates: &'a [S]) -> Self {
        let inputs: Vec<&'a str> = inputs.iter().map(AsRef::as_ref).collect();
        let intermediates: Vec<&'a str> = intermediates.iter().map(AsRef::as_ref).collect();
        let every: Vec<&str> = inputs.iter().chain(&intermediates).copied().collect();
        for (at, name) in every.iter().enumerate() {
            assert!(
                !every[..at].contains(name),
                "{name:?} declared twice: inputs {inputs:?}, intermediates {intermediates:?}"
            );
        }
        Self {
            inputs,
            intermediates,
        }
    }
}

impl<'a, S: AsRef<str>> From<&'a [S]> for Variables<'a> {
    fn from(names: &'a [S]) -> Self {
        Self {
            inputs: names.iter().map(AsRef::as_ref).collect(),
            intermediates: Vec::new(),
        }
    }
}

impl<'a, S: AsRef<str>, const N: usize> From<&'a [S; N]> for Variables<'a> {
    fn from(names: &'a [S; N]) -> Self {
        Self::from(names.as_slice())
    }
}

impl<'a, S: AsRef<str>> From<&'a Vec<S>> for Variables<'a> {
    fn from(names: &'a Vec<S>) -> Self {
        Self::from(names.as_slice())
    }
}

/// Whether a compiled expression comes with its gradient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gradient {
    /// Never: the expression is compiled alone and
    /// [`CompiledExpression::gradient`] is `None`.
    Never,
    /// Where the expression has one: every operator in it has a derivative.
    /// `floor`, `ceil`, `sgn`, `%` and a computed subscript do not, and an
    /// expression using one is compiled without a gradient rather than
    /// refused.
    #[default]
    BestEffort,
}

/// The knobs a compilation has, with a default for each; [`compile`] is the
/// defaults.
///
/// A free function and a builder rather than a method on the tree, because
/// a tree that knows how to compile itself is not a data type. The AST is
/// the shared middle; this module is one of two backends that consume it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Compiler {
    gradient: Gradient,
}

impl Compiler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to compile the gradient beside the expression. The default is
    /// [`Gradient::BestEffort`]; the cost is a second tape of a few times the
    /// size, built once here, so that "compiled" means every register is
    /// allocated and nothing is lowered later.
    #[must_use]
    pub const fn with_gradient(mut self, gradient: Gradient) -> Self {
        self.gradient = gradient;
        self
    }

    /// Parses `source` and binds it to `variables`: the one call that turns
    /// text into something that runs. See [`Variables`] for which rows the
    /// result takes.
    ///
    /// # Errors
    /// [`CompilationFailure`] with every problem found — parsing does not
    /// stop at the first, and a name `variables` lacks is one, at its first
    /// reference.
    pub fn compile<'a>(
        &self,
        source: &str,
        variables: impl Into<Variables<'a>>,
    ) -> Result<CompiledExpression, CompilationFailure> {
        let Variables {
            inputs,
            intermediates,
        } = variables.into();
        let ast = crate::parse(source)?;
        // Only an intermediate the source names gets a row. A subscript
        // cannot reach one, so nothing past this filter can want another.
        let named = intermediates
            .into_iter()
            .filter(|name| ast.symbols().iter().any(|symbol| symbol == name));
        bind(
            &ast,
            &Schema::with_intermediates(inputs, named),
            self.gradient,
        )
    }
}

/// Binds a parsed expression to a schema and lowers it — and its gradient,
/// as `gradient` says.
///
/// This is where missing values are reported — once per schema, rather than on
/// every evaluation as the JVM implementation did.
///
/// A literal subscript names a variable by position and is bound like a
/// name: `var[2]` becomes the second variable — a load, a reference, an
/// incidence row — or a problem at the subscript if there is no second. The
/// caller that resolved already ([`ConstraintSystem::new`](crate::ConstraintSystem::new),
/// which keeps the resolved tree for the search) arrives with nothing left to
/// resolve and pays nothing here.
///
/// # Errors
/// [`CompilationFailure`] naming every symbol the schema omits, each at its
/// first reference in the source, or the first literal subscript it cannot
/// supply, at the subscript.
pub(crate) fn bind(
    ast: &Ast,
    schema: &Schema,
    gradient: Gradient,
) -> Result<CompiledExpression, CompilationFailure> {
    let resolved;
    let ast = if ast.contains_dynamic_lookup() {
        resolved = rewrite::resolve_subscripts(ast.clone(), schema)
            .map_err(|fault| crate::frontend::failure(ast.source(), vec![fault]))?;
        &resolved
    } else {
        ast
    };

    // A symbol nothing references has nowhere to point; the whole
    // expression is the honest fallback.
    let whole = Span::new(
        0,
        u32::try_from(ast.source().chars().count()).unwrap_or(u32::MAX),
    );
    let reference_spans: Vec<Span> = ast
        .reference_spans()
        .into_iter()
        .map(|span| span.unwrap_or(whole))
        .collect();

    let mut global_positions = Vec::with_capacity(ast.symbols().len());
    let mut problems = Vec::new();

    for (symbol, &span) in ast.symbols().iter().zip(&reference_spans) {
        match schema.names.iter().position(|name| name == symbol) {
            Some(position) => {
                let pos = u32::try_from(position).unwrap_or(u32::MAX);
                global_positions.push(pos);
            }
            None => {
                problems.push(Problem::new(
                    ProblemKind::Unbound {
                        name: symbol.clone(),
                    },
                    ast.source(),
                    span,
                ));
            }
        }
    }

    if !problems.is_empty() {
        return Err(CompilationFailure {
            source: ast.source().to_owned(),
            problems,
        });
    }

    // The evaluator's own view of the tree: whole powers as the
    // multiplications they are, which no other consumer wants.
    let program = rewrite::unroll_powers(ast.program().clone());
    let virtual_tape = irgen::emit(&program, &global_positions);
    let partial_diff =
        match gradient {
            Gradient::Never => None,
            Gradient::BestEffort => differentiate::differentiate(&virtual_tape, &global_positions)
                .map(|derivative| CompiledGradient {
                    tape: derivative.allocate(),
                    source: ast.source().to_owned(),
                    schema: schema.clone(),
                    symbols: ast.symbols().to_vec(),
                }),
        };

    Ok(CompiledExpression {
        tape: virtual_tape.allocate(),
        gradient: partial_diff,
        source: ast.source().to_owned(),
        schema: schema.clone(),
        symbols: ast.symbols().to_vec(),
        reference_spans,
        is_constraint: ast.is_constraint(),
        uses_dynamic_lookup: ast.contains_dynamic_lookup(),
    })
}

/// An expression resolved against its variables and lowered, ready to run
/// over a batch.
///
/// **Owned**, not borrowed. It costs one lowering per schema — paid once, at
/// compile time — and buys a type with no lifetime, which matters because
/// `cvg` holds a great many of these inside a worker that outlives every
/// borrow it could have taken.
#[derive(Debug, Clone)]
pub struct CompiledExpression {
    tape: AllocatedTape,
    /// The gradient's tape, compiled alongside as [`Compiler::with_gradient`]
    /// asked.
    gradient: Option<CompiledGradient>,
    /// Held for diagnostics: a runtime failure renders a caret against it.
    source: String,
    /// Gives the expected row count, and the names a failure reports values by.
    schema: Schema,
    /// The names the source refers to, in first-reference order.
    symbols: Vec<String>,
    /// Where each of `symbols` is first referenced, indexed alike.
    reference_spans: Vec<Span>,
    is_constraint: bool,
    uses_dynamic_lookup: bool,
}

impl CompiledExpression {
    /// The source text this was compiled from.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether the source was a boolean expression, and therefore whether the
    /// result should be read as a constraint residual (`<= 0` is satisfied)
    /// rather than as a value.
    #[must_use]
    pub const fn is_constraint(&self) -> bool {
        self.is_constraint
    }

    /// Whether the expression reads a variable by position, `var[i]`.
    ///
    /// A subscript is a one-based index into the inputs in declaration
    /// order, so such an expression can read an input it never names and
    /// [`references`](Self::references) is not the whole story.
    /// **A caller must not prune columns it believes are unreferenced while
    /// this is true** — [`rows`](Self::rows) is the list to build a batch
    /// from, and already accounts for it.
    #[must_use]
    pub const fn uses_dynamic_lookup(&self) -> bool {
        self.uses_dynamic_lookup
    }

    /// The variable names the source mentions.
    #[must_use]
    pub fn references(&self) -> BTreeSet<&str> {
        self.symbols.iter().map(String::as_str).collect()
    }

    /// The rows [`eval`](Self::eval) takes, in order: every input, then the
    /// intermediates the source names, in the order they were listed (see
    /// [`Variables`]).
    #[must_use]
    pub fn rows(&self) -> &[String] {
        self.schema.names()
    }

    /// Each name the source mentions, with the span of its first reference,
    /// in first-reference order: where to put a caret on a problem a caller
    /// finds with a name — a cycle, a boolean read as a number.
    pub fn reference_spans(&self) -> impl Iterator<Item = (&str, Span)> {
        self.symbols
            .iter()
            .map(String::as_str)
            .zip(self.reference_spans.iter().copied())
    }

    /// The gradient of this expression — of its residual, for a constraint —
    /// compiled alongside it, or `None` where the compiler was told not to or
    /// an operator in it has no derivative (`floor`, `ceil`, `sgn`, `%`, a
    /// computed subscript); see [`Gradient`].
    ///
    /// Reverse mode over the tape: one run yields every partial at a cost
    /// that does not grow with the number of variables. See `differentiate.rs`.
    #[must_use]
    pub fn gradient(&self) -> Option<&CompiledGradient> {
        self.gradient.as_ref()
    }

    /// Evaluates one residual per column of `samples`.
    ///
    /// **One column per sample, one row per schema variable.** That is the shape
    /// `cvg` produces, so a generated batch is directly an input matrix with no
    /// transpose — and it is the orientation `faer` stores contiguously, so a
    /// column is a sample laid out end to end.
    ///
    /// **A constraint evaluates to its violation.** A boolean expression —
    /// `a < b`, `a >= b`, `a == b +/- t` — comes back as a scalar whose sign
    /// carries the truth value: negative or zero values `<= 0` holds (the constraint passes),
    /// postive values `> 0` does not hold (the constraint fails), and the size of
    /// a positive value is how badly. That is the `g(x) <= 0` form a
    /// constrained solver takes, so one compiled constraint per row of a
    /// COBYLA-style `constraints` vector is exactly this call. Precisely:
    /// `a <= b` is `a - b`; `a < b` is `a - b` nudged by the smallest positive
    /// `f64`, which vanishes at any real magnitude and separates the two only
    /// at exact equality; `a == b +/- t` is `max(a - b - t, b - a - t)`, the
    /// distance outside the band, negative inside it; a conjunction is the
    /// worst of its terms. An arithmetic expression evaluates to its value.
    ///
    /// Runs a tile of [`TILE`] columns at a time. The first failing column, in
    /// order, is the one reported, naming the innermost subexpression that
    /// went wrong.
    ///
    /// There is no single-sample entry point. There was, and it had no users
    /// outside its own tests: everything that evaluates babel evaluates many
    /// points at once, so a scalar path was a second implementation to keep
    /// correct for nobody's benefit. `Mat::from_fn(n, 1, …)` covers the
    /// one-off case.
    ///
    /// # Errors
    /// [`EvaluationFailure::RowWidthMismatch`] if `samples` has the wrong number of
    /// rows, or [`EvaluationFailure::Runtime`] naming the column and the subexpression
    /// where evaluation failed.
    pub fn eval(&self, samples: MatRef<'_, f64>) -> Result<Col<f64>, EvaluationFailure> {
        self.check_width(samples)?;
        let columns = samples.ncols();
        let mut residuals = Col::zeros(columns);

        let mut tiles = Tiles::new(&self.tape, columns);
        while let Some((first, lanes)) = tiles.next_tile(columns) {
            if let Some((lane, fault)) =
                tiles.run(&self.tape, samples, self.schema.layout(), first, lanes)
            {
                let column = first + lane;
                let row = samples.col(column).iter().copied().collect::<Vec<_>>();
                return Err(runtime_failure(
                    &self.source,
                    &self.schema,
                    &self.tape.fault(fault),
                    Some(column),
                    &row,
                ));
            }
            for (lane, &value) in tiles.results(&self.tape, lanes).iter().enumerate() {
                residuals[first + lane] = value;
            }
        }

        Ok(residuals)
    }

    /// Narrows `holds` to the columns of `samples` on which this constraint is
    /// satisfied: `holds[c]` stays `true` only if it was, and the residual for
    /// column `c` is `<= 0`, and nothing faulted computing it.
    ///
    /// The pool's question, asked of thousands of independent candidates at
    /// once. A candidate whose evaluation faults — `sqrt` of a negative, a
    /// subscript out of range — is a candidate the constraint does not hold
    /// for, and nobody else's problem; it is judged from the fault table, not
    /// from the value, so an intermediate infinity that a later `atan` would
    /// have absorbed still counts. No value leaves this function.
    ///
    /// # Errors
    /// [`EvaluationFailure::RowWidthMismatch`] if `samples` has the wrong number of
    /// rows or `holds` the wrong number of columns.
    pub(crate) fn holds(
        &self,
        samples: MatRef<'_, f64>,
        holds: &mut [bool],
    ) -> Result<(), EvaluationFailure> {
        self.check_width(samples)?;
        let columns = samples.ncols();
        if holds.len() != columns {
            return Err(EvaluationFailure::RowWidthMismatch {
                expected: columns,
                actual: holds.len(),
            });
        }

        let mut tiles = Tiles::new(&self.tape, columns);
        while let Some((first, lanes)) = tiles.next_tile(columns) {
            tiles.run(&self.tape, samples, self.schema.layout(), first, lanes);
            let verdicts = tiles.results(&self.tape, lanes);
            let faults = &tiles.faults[..lanes];
            for (hold, (&residual, fault)) in holds[first..first + lanes]
                .iter_mut()
                .zip(verdicts.iter().zip(faults))
            {
                *hold &= fault.is_none() && residual <= 0.0;
            }
        }
        Ok(())
    }

    /// The tape as a WGSL function named `name`, for the GPU sieve. See
    /// [`wgsl`] for what the text promises and what it does not.
    #[doc(hidden)]
    #[cfg_attr(
        not(feature = "gpu"),
        allow(dead_code, reason = "the GPU sieve is the only caller")
    )]
    pub(crate) fn wgsl(&self, name: &str) -> wgsl::Function {
        wgsl::function(&self.tape, name, self.schema.layout())
    }

    fn check_width(&self, samples: MatRef<'_, f64>) -> Result<(), EvaluationFailure> {
        if samples.nrows() == self.schema.len() {
            Ok(())
        } else {
            Err(EvaluationFailure::RowWidthMismatch {
                expected: self.schema.len(),
                actual: samples.nrows(),
            })
        }
    }

    /// One residual for one row.
    ///
    /// The per-lane executor over the same tape, for the one consumer that
    /// *cannot* batch: the walker's shrinkage loop cannot propose the next
    /// candidate until it has judged this one, so it is sequential by nature,
    /// and wrapping each point in a one-column matrix cost about five times
    /// the evaluation itself — measured, `p118` at 32s against 6s.
    ///
    /// Not a second implementation. It is the same tape with a different loop
    /// around it, which is why it stays private and why there is still no
    /// scalar entry point in the public API.
    ///
    /// # Errors
    /// As [`eval`](Self::eval), without a column to name.
    pub(crate) fn eval_row(&self, row: &[f64]) -> Result<f64, EvaluationFailure> {
        if row.len() != self.schema.len() {
            return Err(EvaluationFailure::RowWidthMismatch {
                expected: self.schema.len(),
                actual: row.len(),
            });
        }
        let mut frame = vec![0.0; self.tape.registers as usize];
        self.tape.prime(&mut frame);
        lane::run_lane(&self.tape, row, self.schema.layout(), &mut frame)
            .map_err(|fault| runtime_failure(&self.source, &self.schema, &fault, None, row))
    }

    /// The tape, for the lowering and allocation tests.
    #[cfg(test)]
    pub(super) fn tape(&self) -> &AllocatedTape {
        &self.tape
    }
}

/// Renders a [`Fault`] into the failure a caller sees.
///
/// The evaluator reports a kind and a location; rendering needs the source,
/// which it deliberately does not carry. Building the [`Problem`] here keeps
/// line and column derived from `span.start` in the one place that does it
/// for syntax errors too.
fn runtime_failure(
    source: &str,
    schema: &Schema,
    fault: &Fault,
    column: Option<usize>,
    row: &[f64],
) -> EvaluationFailure {
    EvaluationFailure::Runtime(Box::new(RuntimeProblem {
        problem: Problem::new(fault.kind.clone(), source, fault.span),
        sample: column,
        // Needs a slot-to-name table the AST deliberately discards.
        locals: Vec::new(),
        parameters: schema
            .names()
            .iter()
            .cloned()
            .zip(row.iter().copied())
            .collect(),
    }))
}

/// The gradient of a compiled expression, as a tape of its own: the primal
/// value and every partial from one run.
///
/// Partials are per symbol the expression names, in the order
/// [`symbols`](Self::symbols) gives — first reference order, which is not
/// the schema's — one row per symbol.
#[derive(Debug, Clone)]
pub struct CompiledGradient {
    tape: AllocatedTape,
    source: String,
    schema: Schema,
    symbols: Vec<String>,
}

impl CompiledGradient {
    /// The symbols the partials are ordered by: `eval`'s rows, by name.
    #[must_use]
    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    /// The Jacobian over a batch: one row per symbol, one column per sample.
    ///
    /// # Errors
    /// As [`CompiledExpression::eval`]; a gradient that is not finite is a
    /// fault where the value may not have been.
    pub fn eval(&self, samples: MatRef<'_, f64>) -> Result<Mat<f64>, EvaluationFailure> {
        if samples.nrows() != self.schema.len() {
            return Err(EvaluationFailure::RowWidthMismatch {
                expected: self.schema.len(),
                actual: samples.nrows(),
            });
        }
        let columns = samples.ncols();
        let mut jacobian = Mat::zeros(self.symbols.len(), columns);

        let mut tiles = Tiles::new(&self.tape, columns);
        while let Some((first, lanes)) = tiles.next_tile(columns) {
            if let Some((lane, fault)) =
                tiles.run(&self.tape, samples, self.schema.layout(), first, lanes)
            {
                let column = first + lane;
                let row = samples.col(column).iter().copied().collect::<Vec<_>>();
                return Err(runtime_failure(
                    &self.source,
                    &self.schema,
                    &self.tape.fault(fault),
                    Some(column),
                    &row,
                ));
            }
            for (symbol, partial) in self.tape.partials.iter().enumerate() {
                for (lane, &value) in tiles.register(*partial, lanes).iter().enumerate() {
                    jacobian[(symbol, first + lane)] = value;
                }
            }
        }

        Ok(jacobian)
    }

    /// The partials at one row.
    ///
    /// # Errors
    /// As [`eval`](Self::eval), without a column to name.
    pub(crate) fn eval_row(&self, row: &[f64]) -> Result<Vec<f64>, EvaluationFailure> {
        self.eval_row_with_value(row).map(|(_, partials)| partials)
    }

    /// The value and the partials at one row, from the one run that computes
    /// both.
    ///
    /// # Errors
    /// As [`eval`](Self::eval), without a column to name.
    pub(crate) fn eval_row_with_value(
        &self,
        row: &[f64],
    ) -> Result<(f64, Vec<f64>), EvaluationFailure> {
        if row.len() != self.schema.len() {
            return Err(EvaluationFailure::RowWidthMismatch {
                expected: self.schema.len(),
                actual: row.len(),
            });
        }
        let mut frame = vec![0.0; self.tape.registers as usize];
        self.tape.prime(&mut frame);
        let value = lane::run_lane(&self.tape, row, self.schema.layout(), &mut frame)
            .map_err(|fault| runtime_failure(&self.source, &self.schema, &fault, None, row))?;
        let partials = self
            .tape
            .partials
            .iter()
            .map(|partial| frame[partial.index()])
            .collect();
        Ok((value, partials))
    }

    /// The tape, for the differentiation tests.
    #[cfg(test)]
    pub(super) fn tape(&self) -> &AllocatedTape {
        &self.tape
    }
}

/// The shape of a row: how many of it are inputs and how many follow them.
///
/// A row has two regions. The **inputs** come first — the design vector,
/// the coordinates a caller chooses, and the only rows `var[i]` may name.
/// The **intermediates** follow — externals and other expressions' outputs,
/// present because an expression names them, bindable by name only. For
/// [`compile`] these are [`Variables`]' two lists, every input followed by
/// the intermediates the source names; for a
/// [`ConstraintSystem`](crate::ConstraintSystem) every row is an input;
/// [`compile_system`](crate::compile_system) lays a node's row out as the
/// inputs it reads followed by what else it names.
///
/// One value rather than two counts, because both go everywhere a row goes
/// — an executor checks the row against [`total`](Self::total) and bounds a
/// gather by `inputs` — and two numbers agreed on by three call sites is
/// how the bound and the width came to be the same thing by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowLayout {
    pub(crate) inputs: usize,
    pub(crate) intermediates: usize,
}

impl RowLayout {
    /// The row's length: inputs and intermediates together.
    #[must_use]
    pub(crate) const fn total(self) -> usize {
        self.inputs + self.intermediates
    }
}

/// An ordered set of variable names: the rows of a batch, with their
/// [`RowLayout`].
///
/// Order is load-bearing: `var[i]` indexes into it, one-based. This is the
/// requirement that forced the JVM API to demand a `LinkedHashMap`.
///
/// Crate-private. A caller states the order by listing names; this is the
/// crate's bookkeeping for that list, not a type anyone has to construct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Schema {
    names: Vec<String>,
    layout: RowLayout,
}

impl Schema {
    /// Builds a schema from names in declaration order, every one an input.
    #[must_use]
    pub(crate) fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let names: Vec<String> = names.into_iter().map(Into::into).collect();
        let layout = RowLayout {
            inputs: names.len(),
            intermediates: 0,
        };
        Self { names, layout }
    }

    /// A schema whose rows are `inputs` followed by `intermediates`: a
    /// subscript indexes the first list and cannot reach the second.
    #[must_use]
    pub(crate) fn with_intermediates<I, J, S>(inputs: I, intermediates: J) -> Self
    where
        I: IntoIterator<Item = S>,
        J: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut names: Vec<String> = inputs.into_iter().map(Into::into).collect();
        let inputs = names.len();
        names.extend(intermediates.into_iter().map(Into::into));
        let layout = RowLayout {
            inputs,
            intermediates: names.len() - inputs,
        };
        Self { names, layout }
    }

    /// A schema over a slice of anything string-like: `&["x1", "x2"]`, a
    /// `Vec<String>`, a caller's own list, for the tests of the pieces.
    #[cfg(test)]
    pub(crate) fn for_names<S: AsRef<str>>(names: &[S]) -> Self {
        Self::new(names.iter().map(AsRef::as_ref))
    }

    /// A schema over the names of a name-value table, for the one-point
    /// evaluators the tests use.
    #[cfg(test)]
    pub(crate) fn for_table(table: &[(&str, f64)]) -> Self {
        Self::new(table.iter().map(|(name, _)| *name))
    }

    /// The row count: inputs and intermediates together.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.names.len()
    }

    /// The row's shape; `layout().inputs` is how many rows a subscript may
    /// name, whatever the row holds after them.
    #[must_use]
    pub(crate) const fn layout(&self) -> RowLayout {
        self.layout
    }

    #[must_use]
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }
}

/// Parses and lowers `source` against `names`, for the tests of the pieces.
#[cfg(test)]
pub(super) fn tape_for(source: &str, names: &[&str]) -> AllocatedTape {
    let ast = crate::parse(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    bind(&ast, &Schema::for_names(names), Gradient::Never)
        .unwrap_or_else(|e| panic!("{source:?} against {names:?}: {e:?}"))
        .tape()
        .clone()
}

/// The batched walk over a straight-line tape: a register file and a fault
/// table sized for one tile and reused across all of them, two allocations
/// per call.
struct Tiles {
    file: RegisterFile,
    faults: Vec<Option<tape::LaneFault>>,
    next: usize,
}

impl Tiles {
    fn new(tape: &AllocatedTape, columns: usize) -> Self {
        let width = columns.min(TILE);
        Self {
            file: RegisterFile::new(tape, width),
            faults: vec![None; width],
            next: 0,
        }
    }

    /// The next tile's first column and width, until the columns run out.
    fn next_tile(&mut self, columns: usize) -> Option<(usize, usize)> {
        if self.next >= columns {
            return None;
        }
        let first = self.next;
        let lanes = (columns - first).min(TILE);
        self.next += lanes;
        Some((first, lanes))
    }

    /// Runs one tile of `samples`, whose rows are laid out as `layout`.
    fn run(
        &mut self,
        tape: &AllocatedTape,
        samples: MatRef<'_, f64>,
        layout: RowLayout,
        first: usize,
        lanes: usize,
    ) -> Option<(usize, tape::LaneFault)> {
        self.faults[..lanes].fill(None);
        tile::run_tile(
            tape,
            samples,
            layout,
            first,
            lanes,
            &mut self.file,
            &mut self.faults[..lanes],
        )
    }

    fn results(&self, tape: &AllocatedTape, lanes: usize) -> &[f64] {
        self.file.reg(tape.result, lanes)
    }

    /// Any register after a run: a differentiated tape's partials.
    fn register(&self, register: Register, lanes: usize) -> &[f64] {
        self.file.reg(register, lanes)
    }
}

/// One expression at one point, as a one-column batch.
///
/// Test plumbing, kept in the crate rather than duplicated across its test
/// modules; the integration tests carry their own copy in `tests/common`. Not
/// a path anything should evaluate through in a loop — it rebuilds a schema,
/// parses, binds, and allocates a row on every call, and the batch API exists
/// precisely so that nothing has to.
#[cfg(test)]
pub(crate) fn eval_one(source: &str, inputs: &[(&str, f64)]) -> Result<f64, EvaluationFailure> {
    let ast = crate::parse(source)?;
    let compiled = bind(&ast, &Schema::for_table(inputs), Gradient::Never)?;
    let sample = faer::Mat::from_fn(inputs.len(), 1, |row, _| inputs[row].1);
    let res = compiled.eval(sample.as_ref())?;
    Ok(res[0])
}

/// [`eval_one`] for a tree already in hand: what a test of a rewrite pass
/// needs, since re-parsing the source would run every pass again and hide
/// the one under test.
#[cfg(test)]
pub(crate) fn eval_parsed(ast: &Ast, inputs: &[(&str, f64)]) -> Result<f64, EvaluationFailure> {
    let schema = Schema::for_table(inputs);
    let compiled = bind(ast, &schema, Gradient::Never)?;
    let sample = faer::Mat::from_fn(inputs.len(), 1, |row, _| inputs[row].1);
    let res = compiled.eval(sample.as_ref())?;
    Ok(res[0])
}

#[cfg(test)]
mod tests {
    use super::eval_one;

    /// An empty range folds to the identity rather than erroring or hanging.
    /// Nothing in the corpus has `lower > upper`.
    #[test]
    fn an_empty_aggregate_range_yields_the_identity() {
        let sum = crate::parse("sum(5, 1, i -> i)").expect("should compile");
        assert_eq!(eval_one(sum.source(), &[]).expect("should evaluate"), 0.0);

        let product = crate::parse("prod(5, 1, i -> i)").expect("should compile");
        assert_eq!(
            eval_one(product.source(), &[]).expect("should evaluate"),
            1.0
        );
    }

    /// A span points at the offending sub-expression, not at the whole
    /// expression. `0/x1` sits at characters 4..8 of `abs(0/x1)`.
    #[test]
    fn a_fault_is_located_at_the_offending_sub_expression() {
        let error = eval_one("abs(0/x1)", &[("x1", 0.0)]).expect_err("0/0 is not a bound");

        match error {
            crate::diagnostics::EvaluationFailure::Runtime(problem) => {
                assert_eq!(problem.problem.span, crate::diagnostics::Span::new(4, 8));
                assert_eq!(problem.problem.line_idx, 0);
                assert_eq!(problem.problem.column_idx, 4);
                // Populated at the boundary, which is the only place that knows
                // the schema.
                assert_eq!(problem.parameters, vec![("x1".to_owned(), 0.0)]);
            }
            other => panic!("expected a runtime problem, got {other:?}"),
        }
    }

    /// Offsets count characters, not bytes. `测试` is two characters and six
    /// UTF-8 bytes, so a byte-based span would report 4..10 here.
    #[test]
    fn spans_count_characters_not_bytes() {
        let error = eval_one("abs(测试) + ln(x1)", &[("测试", 1.5), ("x1", 0.0)])
            .expect_err("ln(0) should fault");

        match error {
            crate::diagnostics::EvaluationFailure::Runtime(problem) => {
                // `ln(x1)` starts after `abs(测试) + `, ten characters in.
                assert_eq!(problem.problem.span, crate::diagnostics::Span::new(10, 16));
                assert_eq!(problem.problem.column_idx, 10);
            }
            other => panic!("expected a runtime problem, got {other:?}"),
        }
    }

    /// Subscripts are one-based, so a computed zero is out of range like any
    /// other miss; only a *written* `var[0]` gets the special case, and that
    /// one never reaches a row.
    #[test]
    fn a_zero_subscript_is_out_of_bounds() {
        let error = eval_one("var[floor(x1) - 7]", &[("x1", 7.0)])
            .expect_err("a subscript computed as zero names nothing");

        match error {
            crate::diagnostics::EvaluationFailure::Runtime(problem) => assert_eq!(
                problem.problem.kind,
                crate::diagnostics::ProblemKind::DynamicIndexOutOfBounds {
                    requested_1index: 0,
                    available: 1,
                }
            ),
            other => panic!("expected a runtime problem, got {other:?}"),
        }
    }
}

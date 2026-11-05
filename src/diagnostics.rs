//! Structured compile-time and run-time diagnostics.
//!
//! # Index naming convention
//!
//! A name ending in **`idx` is zero-based**; a name ending in **`1index` is
//! one-based**. Everything babel reports is zero-based except the `var[i]`
//! subscript, which is one-based in the surface syntax and stays that way.
//!
//! # Thinness
//!
//! ANTLR locates syntax errors; babel's job is to forward them. Every
//! diagnostic the parser emits is reported, verbatim, with source, span, line
//! and column attached — no filtering, no coalescing, no rewording. A parser
//! recovering from a missing paren may well emit several diagnostics for one
//! mistake; deciding which to show a user is the caller's problem, not this
//! module's.
//!
//! Nothing here stores pre-rendered text. [`Display`](std::fmt::Display) builds
//! the human-readable form from the structured data at render time.

use std::fmt;
use std::ops::Range;

/// Half-open range over the source text, measured in Unicode scalar values.
///
/// Character offsets rather than UTF-8 byte offsets: Babel accepts Unicode
/// identifiers, and consumers place carets by character, not by byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// Converts a UTF-8 byte range — the form the parse layer reports spans in
    /// — into character offsets.
    ///
    /// Byte offsets past the end of `source`, which recovery can produce, clamp
    /// to the end rather than panicking.
    #[must_use]
    pub fn from_utf8_range(source: &str, bytes: Range<usize>) -> Self {
        let to_chars = |byte: usize| -> u32 {
            let byte = byte.min(source.len());
            // Round down to a char boundary; a mid-character offset would
            // otherwise panic on slicing.
            let mut at = byte;
            while at > 0 && !source.is_char_boundary(at) {
                at -= 1;
            }
            u32::try_from(source[..at].chars().count()).unwrap_or(u32::MAX)
        };
        Self::new(to_chars(bytes.start), to_chars(bytes.end))
    }
}

/// Zero-based line and column of a character offset.
///
/// The single place either is computed, so the two can never disagree — the
/// JVM implementation derived them separately and drifted.
#[must_use]
pub fn line_col_idx(source: &str, char_idx: u32) -> (u32, u32) {
    let mut line_idx = 0;
    let mut column_idx = 0;
    for ch in source.chars().take(char_idx as usize) {
        if ch == '\n' {
            line_idx += 1;
            column_idx = 0;
        } else {
            column_idx += 1;
        }
    }
    (line_idx, column_idx)
}

/// Which aggregate bound was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundKind {
    Lower,
    Upper,
}

impl fmt::Display for BoundKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lower => "lower",
            Self::Upper => "upper",
        })
    }
}

/// What went wrong.
#[derive(Debug, Clone, PartialEq)]
pub enum ProblemKind {
    /// The supplied source text was empty.
    EmptyExpression,

    /// A diagnostic from ANTLR, forwarded as reported.
    ///
    /// `message` is the runtime's own wording. The parser builds a structured
    /// `MismatchedInput { expected, found }` internally but formats it into a
    /// string before any listener sees it, so the expected-token set is only
    /// available here as prose.
    ///
    /// `from_lexer` is the one piece of classification that comes for free: the
    /// lexer reports no offending token, the parser always does. It separates
    /// "that character cannot start a token" from "that construct is wrong"
    /// without inspecting the message.
    Syntax { message: String, from_lexer: bool },

    /// A construct babel parses but this build cannot lower yet.
    Unsupported { feature: String },

    /// The expression names a variable nothing supplies: not in the list a
    /// [`compile`](crate::compile) was given, not declared in a
    /// [`ConstraintSystem`](crate::ConstraintSystem)'s box. Located at the
    /// name's first reference; a name used twice is one problem.
    Unbound { name: String },

    // ---- defined, but nothing produces these until the features land ----
    /// A `sum`/`prod` bound is a constant that is not a usable index: NaN,
    /// infinite, or fractional.
    IllegalAggregateBound { bound: BoundKind, value: f64 },
    /// A `sum`/`prod` bound that is not a constant expression. Aggregates are
    /// big-sigma over a fixed index set and are unrolled at compile time, so a
    /// bound that depends on a variable has no meaning here.
    AggregateBoundNotConstant { bound: BoundKind },
    /// A `sum`/`prod` spanning more terms than will be unrolled.
    AggregateTooWide { terms: i64, limit: i64 },
    /// `var[i]` addressed a parameter that does not exist.
    DynamicIndexOutOfBounds {
        requested_1index: i64,
        available: usize,
    },
    /// `var[i]` was given a literal that is not a whole number: `var[1.5]`.
    /// A computed subscript never gets this far — see
    /// [`SubscriptNotIntegral`](Self::SubscriptNotIntegral).
    DynamicIndexNotAnInteger { value: f64 },
    /// A subscript the row decides that is not a whole number by
    /// construction: `var[x2]`, `var[i/2]`, `var[x1 - 0.5]`. The forms that
    /// are — `floor`/`ceil` of anything, an aggregate's parameter, exact
    /// integer arithmetic over those — are the table on `ast::is_integral`
    /// (crate-private, the one type judgement); a subscript in that
    /// shape cannot be un-integral by rounding, which is why there is no
    /// runtime check behind this one. The JVM implementation rounded
    /// silently instead, so `var[1.7]` read `var[2]`.
    SubscriptNotIntegral,
    /// `var[0]`, written as such. Subscripts are one-based, and zero is the
    /// one mistake common enough to answer with the fix: the first parameter
    /// is `var[1]`. Known from the source alone, so reported at compile time,
    /// where [`DynamicIndexOutOfBounds`](Self::DynamicIndexOutOfBounds) needs
    /// a row.
    ZeroIndex,
    /// `var[i]` was given a negative literal, which no schema can satisfy.
    /// Compile time, like [`ZeroIndex`](Self::ZeroIndex).
    NegativeDynamicIndex { requested_1index: i64 },

    /// A subexpression made only of constants works out to NaN or an infinity.
    ///
    /// `sqrt(-1)`, `1/0`, `0/0`, and a literal too large for `f64` such as
    /// `1.0e400`. Rejected rather than folded, because an expression that can
    /// only ever be non-finite is a mistake worth reporting at the span where
    /// it was written rather than a NaN surfacing somewhere downstream.
    NonFiniteConstant { value: f64 },

    /// An equality's `+/-` tolerance is zero or negative.
    ///
    /// `a == b +/- 0` asks for exact `f64` equality: the feasible set is
    /// whatever pairs of doubles happen to land on `b` exactly, a scatter of
    /// points with no volume that rejection sampling cannot reach and a
    /// solver's real-valued witness usually misses by an ulp. A negative
    /// tolerance is satisfied by nothing at all. Judged on the value, not the
    /// spelling, so `0`, `0.0`, `-0.0` and `0.0e1` are all refused; a tiny
    /// positive tolerance below the ulp of the values compared has the same
    /// problem and cannot be caught here, because it depends on the values.
    DegenerateTolerance { tolerance: f64 },

    /// `a implies b implies c`, with neither implication bracketed. Located
    /// at the second `implies`.
    ///
    /// Refused because every reading of it is someone's surprise: logic
    /// associates it to the right, `a implies (b implies c)`, which is `(a
    /// and b) implies c`; ANTLR's default would be the left, `(a implies b)
    /// implies c`, which is `(a and not b) or c` and which nobody means; and
    /// a reader of `a < b < c` expects a transitive chain, a third meaning.
    /// Brackets on either side say which, and both readings compile.
    ChainedImplication,

    /// A subexpression evaluated to NaN or an infinity for this row.
    ///
    /// The same rule as [`ProblemKind::NonFiniteConstant`], for the values that
    /// only a row can reveal: `ln(x)` at `x = 0`, `sqrt(x)` at `x = -1`,
    /// overflow, or a non-finite input handed in by the caller.
    ///
    /// Reported at the *innermost* subexpression that produced it rather than
    /// at the whole expression, which is why the evaluator checks every node
    /// rather than only its own result. A non-finite value that is allowed to
    /// travel loses the one piece of information worth having about it.
    NonFiniteValue { value: f64 },

    // ---- the graph: only `compile_system` produces these ----
    /// A name declared twice — in one of the three lists
    /// [`compile_system`](crate::compile_system) takes, or across two of
    /// them. Located at the second definition where that is an expression;
    /// a clash between two declared names has no source to point at.
    Duplicate { name: String },
    /// An expression reads an output whose expression is a constraint. A
    /// boolean has no value — its residual is the evaluator's convention,
    /// not a number the author wrote — so `c2 * 2` is a mistake, not `0`
    /// or `1`. Located at the reference.
    ConstraintAsValue { name: String },
    /// An output depends on itself, directly (`h: h + 1`) or round a chain.
    /// `chain` is the outputs round the cycle starting from the expression
    /// the problem is reported in, which is the one whose reference closed
    /// it: `g1: g2 + 1`, `g2: g1 + 1` reports under `g2` with `[g2, g1]`,
    /// read `g2 -> g1 -> g2`. Located at that reference.
    Cycle { chain: Vec<String> },
}

impl ProblemKind {
    /// The clause after `Error in '…': `.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::EmptyExpression => "expression is empty".to_owned(),
            Self::Syntax { .. } => "syntax error".to_owned(),
            Self::Unsupported { feature } => format!("{feature} is not supported yet"),
            Self::Unbound { .. } => "unknown variable".to_owned(),
            Self::IllegalAggregateBound { bound, .. } => format!("illegal {bound} bound value"),
            Self::AggregateBoundNotConstant { bound } => {
                format!("the {bound} bound of a sum or prod must be a constant")
            }
            Self::AggregateTooWide { terms, limit } => {
                format!("a sum or prod over {terms} terms is wider than the {limit} supported")
            }
            Self::DynamicIndexOutOfBounds {
                requested_1index,
                available,
            } => {
                let magnitude = requested_1index.abs();
                let suffix = match (magnitude % 100, magnitude % 10) {
                    (11..=13, _) => "th",
                    (_, 1) => "st",
                    (_, 2) => "nd",
                    (_, 3) => "rd",
                    _ => "th",
                };
                format!(
                    "attempted to access 'var[{requested_1index}]' \
                         (the {requested_1index}{suffix} parameter) \
                         when only {available} exist"
                )
            }
            Self::DynamicIndexNotAnInteger { .. } => {
                "attempted to use a non-integer as an index".to_owned()
            }
            Self::SubscriptNotIntegral => {
                "this subscript is not a whole number by construction".to_owned()
            }
            Self::ZeroIndex => {
                "var[0] is not the first parameter (did you mean var[1]?)".to_owned()
            }
            Self::NegativeDynamicIndex { requested_1index } => {
                format!("attempted to access 'var[{requested_1index}]', but subscripts start at 1")
            }
            Self::NonFiniteConstant { value } => {
                let what = if value.is_nan() { "NaN" } else { "infinite" };
                format!("this is constantly {what}")
            }
            Self::DegenerateTolerance { tolerance } => {
                if *tolerance < 0.0 {
                    "a negative tolerance is satisfied by nothing".to_owned()
                } else {
                    "a tolerance of zero is exact floating-point equality, which has no \
                     volume to sample; give the band a width"
                        .to_owned()
                }
            }
            Self::ChainedImplication => "chained `implies` is ambiguous".to_owned(),
            Self::NonFiniteValue { value } => {
                let what = if value.is_nan() {
                    "not a number"
                } else {
                    "infinite"
                };
                format!("this evaluated to something {what}")
            }
            Self::Duplicate { name } => format!("'{name}' is declared more than once"),
            Self::ConstraintAsValue { name } => {
                format!("'{name}' is a constraint and has no value to read")
            }
            Self::Cycle { chain } => {
                let round = chain
                    .iter()
                    .chain(chain.first())
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                format!(
                    "'{}' depends on itself: {round}",
                    chain.first().map_or("", String::as_str)
                )
            }
        }
    }

    /// The note printed after the underline. Empty when there is nothing to add.
    #[must_use]
    pub fn annotation(&self) -> String {
        match self {
            Self::EmptyExpression
            | Self::Unsupported { .. }
            | Self::ZeroIndex
            | Self::AggregateBoundNotConstant { .. }
            | Self::AggregateTooWide { .. }
            | Self::Duplicate { .. }
            | Self::Cycle { .. } => String::new(),
            Self::ConstraintAsValue { .. } => {
                "a boolean is not a number; compare against the expression instead".to_owned()
            }
            Self::Syntax { message, .. } => message.clone(),
            Self::Unbound { .. } => "no input variable by that name".to_owned(),
            Self::SubscriptNotIntegral => "wrap it in floor() or ceil() to say which".to_owned(),
            Self::ChainedImplication => {
                "bracket one side: `a implies (b implies c)` or `(a implies b) implies c`"
                    .to_owned()
            }
            Self::IllegalAggregateBound { value, .. }
            | Self::DynamicIndexNotAnInteger { value }
            | Self::NonFiniteConstant { value }
            | Self::NonFiniteValue { value }
            | Self::DegenerateTolerance { tolerance: value } => format!("evaluates to {value}"),
            Self::DynamicIndexOutOfBounds {
                requested_1index, ..
            }
            | Self::NegativeDynamicIndex { requested_1index } => {
                format!("evaluates to {requested_1index}")
            }
        }
    }
}

/// A failure that knows *what* and *where*, but not how to render itself.
///
/// Neither the evaluator nor the rewrite passes carry the source text — the
/// evaluator because a tape running a tile of samples has no business threading
/// a string through its loops, the passes because they have no reason to. Both
/// report this, and the boundary that does have the source turns it into a
/// [`Problem`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fault {
    pub kind: ProblemKind,
    pub span: Span,
}

/// One problem, located in the source text it was found in.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub kind: ProblemKind,
    /// The full source, so `Display` needs no other context.
    pub source: String,
    /// The offending text, exactly as located.
    pub span: Span,
    /// Zero-based line index of `span.start`.
    pub line_idx: u32,
    /// Zero-based character index within that line.
    pub column_idx: u32,
}

impl Problem {
    /// Builds a problem, deriving line and column from `span.start`.
    #[must_use]
    pub fn new(kind: ProblemKind, source: &str, span: Span) -> Self {
        let (line_idx, column_idx) = line_col_idx(source, span.start);
        Self {
            kind,
            source: source.to_owned(),
            span,
            line_idx,
            column_idx,
        }
    }

    /// The offending source text. Empty for a zero-width span, which is how
    /// end-of-input is reported.
    #[must_use]
    pub fn text(&self) -> String {
        self.source
            .chars()
            .skip(self.span.start as usize)
            .take(self.span.end.saturating_sub(self.span.start) as usize)
            .collect()
    }

    // The `    ~~~ note` line placed under the offending text.
}

/// `{}` is a one-line summary; `{:#}` is the full block, with the source and a
/// caret under the offending text.
///
/// That split follows the standard library's use of the alternate flag —
/// `{:?}` versus `{:#?}`, `{:x}` versus `{:#x}` — where alternate always means
/// the more expanded form. A caller writing a log line wants the first; a
/// caller showing a person wants the second, and should assume a monospace
/// font.
impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.text();
        let subject = if text.is_empty() {
            "end of expression"
        } else {
            &text
        };
        let summary = self.kind.summary();
        let annotation = self.kind.annotation();

        if !f.alternate() {
            // An empty source has nowhere to point, so naming a location would
            // be noise.
            if self.source.is_empty() {
                return f.write_str(&summary);
            }
            write!(f, "{summary} at '{subject}'")?;
            return if annotation.is_empty() {
                Ok(())
            } else {
                write!(f, ": {annotation}")
            };
        }

        let mut out = vec![format!("Error in '{subject}': {summary}.")];
        for (idx, line) in self.source.lines().enumerate() {
            out.push(line.to_owned());
            if idx as u32 == self.line_idx {
                let line_len = line.chars().count();

                // A zero-width span means end-of-input. Underline the final
                // character rather than drawing nothing — a rendering decision,
                // deliberately kept out of the data so the reported span stays
                // exactly as located.
                let width = self.span.end.saturating_sub(self.span.start).max(1) as usize;
                let mut column = self.column_idx as usize;
                if column >= line_len && line_len > 0 {
                    column = line_len - 1;
                }

                let underline = format!("{}{}", " ".repeat(column), "~".repeat(width));
                out.push(if annotation.is_empty() {
                    underline
                } else {
                    format!("{underline} {annotation}")
                });
            }
        }
        f.write_str(&out.join("\n"))
    }
}

/// The state snapshot only appears under `{:#}` — it is the verbose half, and a
/// log line does not want a hundred parameters in it.
impl fmt::Display for RuntimeProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !f.alternate() {
            return write!(f, "{}", self.problem);
        }

        writeln!(f, "{:#}", self.problem)?;
        writeln!(f, "local-variables{{{}}}", join_bindings(&self.locals))?;
        write!(f, "parameters{{{}}}", join_bindings(&self.parameters))
    }
}

impl fmt::Display for CompilationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Blocks need a blank line between them to stay readable; one-liners do
        // not.
        let (rendered, separator): (Vec<String>, &str) = if f.alternate() {
            (
                self.problems.iter().map(|p| format!("{p:#}")).collect(),
                "\n\n",
            )
        } else {
            (
                self.problems.iter().map(ToString::to_string).collect(),
                "\n",
            )
        };
        f.write_str(&rendered.join(separator))
    }
}

/// A problem raised during evaluation, with the state that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeProblem {
    pub problem: Problem,
    /// Which column of the batch failed.
    ///
    /// The parameters below say *what* the values were; this says *where* in a
    /// batch of ten thousand to find them again.
    pub sample: Option<usize>,
    /// Lambda parameters and `var x = …` bindings in scope at the failure.
    pub locals: Vec<(String, f64)>,
    /// The bound schema's values.
    pub parameters: Vec<(String, f64)>,
}

fn join_bindings(bindings: &[(String, f64)]) -> String {
    bindings
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Compilation produced no evaluable expression: every problem found, each
/// with its span, whether the text did not parse or it parsed and named a
/// variable nothing supplies ([`ProblemKind::Unbound`], at the name's first
/// reference). One type for everything between source text and a bound
/// expression, since every problem on that road has somewhere to point.
#[derive(Debug, Clone, PartialEq)]
pub struct CompilationFailure {
    pub source: String,
    pub problems: Vec<Problem>,
}

impl std::error::Error for CompilationFailure {}

/// One problem and the declared name it belongs to.
///
/// `name` is the output whose expression the problem was found in — a parse
/// error, an unbound name, a cycle closed there — or, for a
/// [`Duplicate`](ProblemKind::Duplicate) between two declared names with no
/// expression to point at, the name declared twice. The [`Problem`] carries
/// its own source and span as ever; this pairs it with where in the
/// caller's document to say so.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedProblem {
    pub name: String,
    pub problem: Problem,
}

/// [`compile_system`](crate::compile_system) produced no nodes: every
/// problem it could find across every expression, each under the name it
/// belongs to.
///
/// A separate type from [`CompilationFailure`] rather than a field on
/// [`Problem`]: a failure over one source text has one source, and a
/// failure over a document has as many as it has expressions, so the two
/// are different shapes, and an `Option<name>` on every problem would make
/// "which one" a question with a wrong answer available.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemCompilationFailure {
    pub problems: Vec<NamedProblem>,
}

/// `{}` is one line per problem, `name: summary at 'text': note`; `{:#}` is
/// a block per problem headed by its name, with the caret.
impl fmt::Display for SystemCompilationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (rendered, separator): (Vec<String>, &str) = if f.alternate() {
            (
                self.problems
                    .iter()
                    .map(|named| format!("{}:\n{:#}", named.name, named.problem))
                    .collect(),
                "\n\n",
            )
        } else {
            (
                self.problems
                    .iter()
                    .map(|named| format!("{}: {}", named.name, named.problem))
                    .collect(),
                "\n",
            )
        };
        f.write_str(&rendered.join(separator))
    }
}

impl std::error::Error for SystemCompilationFailure {}

/// Evaluation failed.
#[derive(Debug, Clone, PartialEq)]
pub enum EvaluationFailure {
    /// The source never became an expression bound to the inputs given.
    Compile(CompilationFailure),
    /// A problem arose while evaluating.
    Runtime(Box<RuntimeProblem>),
    /// The row handed to `evaluate` did not match the bound schema's width.
    RowWidthMismatch { expected: usize, actual: usize },
}

impl fmt::Display for EvaluationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(e) => {
                if f.alternate() {
                    write!(f, "{e:#}")
                } else {
                    write!(f, "{e}")
                }
            }
            // Forward the flag: a runtime failure rendered with `{:#}` should
            // get the block and the state snapshot, not just the summary.
            Self::Runtime(p) => {
                if f.alternate() {
                    write!(f, "{p:#}")
                } else {
                    write!(f, "{p}")
                }
            }
            Self::RowWidthMismatch { expected, actual } => {
                write!(f, "expected a row of {expected} value(s), got {actual}")
            }
        }
    }
}

impl std::error::Error for EvaluationFailure {}

impl From<CompilationFailure> for EvaluationFailure {
    fn from(failure: CompilationFailure) -> Self {
        Self::Compile(failure)
    }
}

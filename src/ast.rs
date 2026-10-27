//! Babel's abstract syntax tree.
//!
//! ANTLR 4 produces a *parse* tree and offers no way to rewrite it — the tree
//! rewriting that ANTLR 3 supported was deliberately removed, and Parr's
//! recommendation for v4 is to build your own model with a visitor. This module
//! is that model. Everything downstream of [`crate::frontend::parse`] works here, not on
//! ANTLR contexts.
//!
//! Two invariants shape the design:
//!
//! 1. **Symbols are resolved during lowering**, not at evaluation time. Names
//!    become [`GlobalId`] or [`LocalSlot`], so shadowing (`sum(1,3,x1 -> x1) + x1`)
//!    is settled structurally and evaluation needs no scope chain.
//! 2. **Every node carries a [`Span`]**, because diagnostics are reported against
//!    arbitrary sub-expressions at both compile time and run time.

use crate::diagnostics::Span;

/// Position of a variable in the bound [`Schema`](crate::Schema)'s declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GlobalId(u32);

impl GlobalId {
    /// Callers treat this as an opaque handle. That it happens to be a dense
    /// index into a `Vec` is [`Schema`](crate::Schema)'s business — the schema
    /// presents as a fast map and is free to change how it stores things.
    pub(crate) const fn from_index(index: u32) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Position of a value in the current evaluation frame — a `var x = …` binding
/// or a lambda parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalSlot(u32);

impl LocalSlot {
    /// Slots are handed out monotonically during translation and never reused,
    /// so one flat frame serves the whole tree. Opaque for the same reason
    /// [`GlobalId`] is: that it indexes a `Vec` is the evaluator's business.
    pub(crate) const fn from_index(index: u32) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A complete compiled expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub body: Block,
    /// Number of local slots the deepest frame requires. Lets evaluation
    /// allocate one flat frame instead of growing a scope chain.
    pub frame_size: u32,
}

/// `(statement ';')* returnStatement ';'?`
///
/// The grammar guarantees a trailing result expression, so this is a struct
/// with a required `result` rather than a list of statements that might not
/// produce a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub assignments: Vec<Assignment>,
    pub result: Expr,
}

/// `var <name> = <expr>`
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub slot: LocalSlot,
    pub value: Expr,
    pub span: Span,
}

/// An expression node: a [`Kind`] plus its source location.
///
/// The split follows `rustc_ast::Expr { kind, span, .. }` — it keeps `match`
/// arms free of span noise while guaranteeing every node has one.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: Kind,
    pub span: Span,
}

impl Expr {
    #[must_use]
    pub fn new(kind: Kind, span: Span) -> Self {
        Self { kind, span }
    }

    /// The expressions directly under this one, in source order — the one
    /// statement of the tree's shape. A new [`Kind`] is added here and
    /// nowhere else for a traversal to see it; the match is exhaustive so the
    /// compiler says where.
    #[must_use]
    pub(crate) fn children(&self) -> Vec<&Expr> {
        match &self.kind {
            Kind::Literal(_) | Kind::Global(_) | Kind::Local(_) => Vec::new(),
            Kind::Unary { arg, .. } | Kind::DynamicIndex(arg) => vec![arg],
            Kind::Binary { lhs, rhs, .. }
            | Kind::Compare { lhs, rhs, .. }
            | Kind::NearEq { lhs, rhs, .. } => vec![lhs, rhs],
            Kind::And { terms } | Kind::Fold { terms, .. } => terms.iter().collect(),
            Kind::Block(block) => block.expressions().collect(),
            Kind::Aggregate {
                lower, upper, body, ..
            } => [lower.as_ref(), upper.as_ref()]
                .into_iter()
                .chain(body.expressions())
                .collect(),
        }
    }

    /// This node and everything under it, parents before children, left to
    /// right — source order. For a query: what is read, whether a kind
    /// appears, how many times, where first. A transform that builds a tree
    /// recurses instead, since its recursion *is* the traversal state, in
    /// the language's own syntax. Borrows, so nothing changes through it.
    pub(crate) fn iter_preorder(&self) -> Preorder<'_> {
        Preorder { stack: vec![self] }
    }
}

/// [`Expr::iter_preorder`]: an explicit stack, the next node on top.
pub(crate) struct Preorder<'a> {
    stack: Vec<&'a Expr>,
}

impl<'a> Iterator for Preorder<'a> {
    type Item = &'a Expr;

    fn next(&mut self) -> Option<&'a Expr> {
        let next = self.stack.pop()?;
        // Reversed, so the leftmost child is popped first.
        self.stack.extend(next.children().into_iter().rev());
        Some(next)
    }
}

impl Block {
    /// The block's expressions in source order: each assignment's value,
    /// then the result.
    pub(crate) fn expressions(&self) -> impl Iterator<Item = &Expr> {
        self.assignments
            .iter()
            .map(|assignment| &assignment.value)
            .chain(std::iter::once(&self.result))
    }

    /// [`Expr::iter_preorder`] over every expression of the block, in order.
    pub(crate) fn iter_preorder(&self) -> impl Iterator<Item = &Expr> {
        self.expressions().flat_map(Expr::iter_preorder)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Literal(f64),

    /// A statically named variable, resolved against the schema.
    Global(GlobalId),
    /// A `var x = …` binding or a lambda parameter.
    Local(LocalSlot),
    /// `var[expr]` — a one-based index into schema declaration order, computed
    /// at run time. This is what makes [`crate::Schema`] ordered.
    DynamicIndex(Box<Expr>),

    Unary {
        op: UnaryOp,
        arg: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },

    /// `sum(lower, upper, param -> body)` / `prod(…)`.
    Aggregate {
        kind: AggregateKind,
        lower: Box<Expr>,
        upper: Box<Expr>,
        param: LocalSlot,
        body: Box<Block>,
    },

    /// An aggregate whose bounds were known at compile time, unrolled into its
    /// terms by [`crate::frontend::rewrite::unroll_aggregates`].
    ///
    /// N-ary rather than a chain of [`Kind::Binary`]: a thousand-term
    /// aggregate is one node deep rather than a thousand, which keeps the
    /// recursive passes — lowering, interval narrowing — off the stack limit,
    /// and a fold is what the runtime loop was anyway.
    ///
    /// Evaluated left-to-right from [`AggregateKind::identity`], which is what
    /// the runtime loop does — so unrolling cannot change a result. Rebalancing
    /// would, since `f64` addition is not associative.
    Fold {
        kind: AggregateKind,
        terms: Vec<Expr>,
    },

    /// A multi-statement lambda body used in expression position.
    Block(Box<Block>),

    // ---- the boolean variants ----
    //
    // These survive compilation. They used to be flattened into arithmetic by a
    // `rewrite_booleans` pass in the shared pipeline, which meant the `<= 0`
    // residual convention — *the evaluator's* convention — destroyed the
    // structure `cvg` needs before `cvg` could read it. Each backend lowers
    // them its own way now: `eval` computes a residual inline, interval
    // narrowing reads a comparison as a target interval.
    //
    // The grammar keeps them at the root of an expression and nowhere else:
    // `lambdaExpr` takes a `scalarBlock`, so a boolean cannot appear inside
    // arithmetic.
    /// `a < b`, `a >= b`, …
    Compare {
        op: CompareOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `a == b +/- tolerance`.
    NearEq {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        tolerance: f64,
    },
    /// Every term holding at once.
    ///
    /// Not something the grammar produces — babel has no `and`. It exists
    /// because [`crate::frontend::rewrite::invert_monotone`] needs a conjunction
    /// for its domain guard: `ln(x) < 2` means `x < e^2` **and** `x > 0`.
    ///
    /// It used to build `max(residual_a, residual_b) <= 0` by hand, which meant
    /// the front end knew the residual convention. A variant it can emit
    /// without knowing costs a match arm in each backend and buys the
    /// separation outright.
    And {
        terms: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Cos,
    Sin,
    Tan,
    Acos,
    Asin,
    Atan,
    Cosh,
    Sinh,
    Tanh,
    Cot,
    Ln,    // Natural logarithm (log base e) -- Babel's `ln`
    Log10, // Base-10 logarithm -- Babel's unary `log`
    Abs,
    Sqrt,
    Cbrt,
    Sqr,
    Cube,
    Ceil,
    Floor,
    Sgn,
}

impl UnaryOp {
    /// Maps a `unaryFunction` keyword to its operator.
    ///
    /// Lives here rather than in the front end because it constructs this type
    /// and has to stay in step with its variants.
    #[must_use]
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Some(match keyword {
            "cos" => UnaryOp::Cos,
            "sin" => UnaryOp::Sin,
            "tan" => UnaryOp::Tan,
            "acos" => UnaryOp::Acos,
            "asin" => UnaryOp::Asin,
            "atan" => UnaryOp::Atan,
            "cosh" => UnaryOp::Cosh,
            "sinh" => UnaryOp::Sinh,
            "tanh" => UnaryOp::Tanh,
            "cot" => UnaryOp::Cot,
            // Babel renames Java's log/log10 to ln/log respectively.
            "ln" => UnaryOp::Ln,
            "log" => UnaryOp::Log10,
            "abs" => UnaryOp::Abs,
            "sqrt" => UnaryOp::Sqrt,
            "cbrt" => UnaryOp::Cbrt,
            "sqr" => UnaryOp::Sqr,
            "cube" => UnaryOp::Cube,
            "ceil" => UnaryOp::Ceil,
            "floor" => UnaryOp::Floor,
            "sgn" => UnaryOp::Sgn,
            _ => return None,
        })
    }
}

impl UnaryOp {
    /// Applies the operator. Lives here rather than in the evaluator because
    /// the meaning of an operator belongs with the operator, and constant
    /// folding needs it too.
    #[must_use]
    pub fn apply(self, x: f64) -> f64 {
        match self {
            Self::Negate => -x,
            Self::Cos => x.cos(),
            Self::Sin => x.sin(),
            Self::Tan => x.tan(),
            Self::Acos => x.acos(),
            Self::Asin => x.asin(),
            Self::Atan => x.atan(),
            Self::Cosh => x.cosh(),
            Self::Sinh => x.sinh(),
            Self::Tanh => x.tanh(),
            Self::Cot => 1.0 / x.tan(),
            Self::Ln => x.ln(),
            Self::Log10 => x.log10(),
            Self::Abs => x.abs(),
            Self::Sqrt => x.sqrt(),
            Self::Cbrt => x.cbrt(),
            Self::Sqr => x * x,
            Self::Cube => x * x * x,
            Self::Ceil => x.ceil(),
            Self::Floor => x.floor(),
            // Java's Math.signum returns +/-0.0 for +/-0.0 and NaN for NaN;
            // Rust's f64::signum returns 1.0 for +0.0 and -1.0 for -0.0 and NaN.
            // Babel's semantics are Java's, so preserve them.
            Self::Sgn => {
                if x == 0.0 || x.is_nan() {
                    x
                } else {
                    x.signum()
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    /// `a % b` — a *remainder*, not a modulo, and the distinction is not
    /// pedantry. The sign follows the dividend, so `-7 % 3` is -1; a modulo
    /// takes the sign of the divisor and would answer 2. Babel follows Java
    /// here, as `apply` does. The grammar's token is still `MOD` and the
    /// operator is still spelled `%`, because both are the JVM's and neither
    /// is ours to rename.
    Rem,
    Pow,
    Max,
    Min,
    /// `log(base, x)` — Babel's binary `log`.
    LogB,
}

impl BinaryOp {
    /// Maps a `binaryFunction` keyword to its operator. Only the three
    /// call-syntax functions; the infix operators come from the grammar's
    /// operator rules instead.
    #[must_use]
    pub fn from_function_keyword(keyword: &str) -> Option<Self> {
        Some(match keyword {
            "max" => BinaryOp::Max,
            "min" => BinaryOp::Min,
            "log" => BinaryOp::LogB,
            _ => return None,
        })
    }
}

impl BinaryOp {
    /// Applies the operator.
    #[must_use]
    pub fn apply(self, a: f64, b: f64) -> f64 {
        match self {
            Self::Add => a + b,
            Self::Sub => a - b,
            Self::Mul => a * b,
            Self::Div => a / b,
            // Rust's `%` on f64 is the truncated remainder, which is
            // exactly Java's. No adjustment needed.
            Self::Rem => a % b,
            Self::Pow => a.powf(b),
            // Java's Math.max/min propagate NaN and order the signed zeros;
            // Rust's f64::max/min discard NaN and, on this toolchain, answer
            // `max(-0.0, 0.0)` one way when constant-folded and the other way
            // at run time. Babel's semantics are Java's, spelled out.
            Self::Max => nan_or(a, b, java_max),
            Self::Min => nan_or(a, b, java_min),
            // log(base, x) == ln(x) / ln(base)
            Self::LogB => b.ln() / a.ln(),
        }
    }
}

fn nan_or(a: f64, b: f64, f: impl Fn(f64, f64) -> f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        f(a, b)
    }
}

/// `Math.max` for two non-NaN doubles: the larger, and on equal values the
/// one with the positive sign — which only differs from "either" for `-0.0`
/// against `0.0`, where Java answers `0.0`. Two equal values that are not
/// zeros are bitwise identical, so the sign rule cannot pick wrong there.
fn java_max(a: f64, b: f64) -> f64 {
    if a > b {
        a
    } else if b > a || a.is_sign_negative() {
        b
    } else {
        a
    }
}

/// `Math.min`: the mirror of [`java_max`], answering `-0.0` for the zeros.
fn java_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else if a < b || a.is_sign_negative() {
        a
    } else {
        b
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Sum,
    Prod,
}

impl AggregateKind {
    /// The fold's identity: `0` for `sum`, `1` for `prod`.
    #[must_use]
    pub const fn identity(self) -> f64 {
        match self {
            Self::Sum => 0.0,
            Self::Prod => 1.0,
        }
    }

    /// Accumulates one term.
    #[must_use]
    pub fn combine(self, accumulated: f64, term: f64) -> f64 {
        match self {
            Self::Sum => accumulated + term,
            Self::Prod => accumulated * term,
        }
    }
}

/// The single place where an `f64` becomes an integer index.
///
/// Babel conflates integer and floating-point maths: `sum`/`prod` bounds and
/// `var[i]` subscripts are all `f64`. Rather than give indices their own type,
/// every conversion routes through here, so there is exactly one place that
/// decides what counts as an index.
///
/// Returns `None` for NaN, infinities, anything with a fractional part, and
/// anything beyond ±2^53 — past which `f64` cannot represent consecutive
/// integers, so "integral" stops meaning anything. Callers attach the
/// diagnostic, since only they know whether a failure is a bad bound or a bad
/// subscript.
///
/// Deliberately strict. The JVM implementation rounded, so `sum(1, 20/3, ...)`
/// silently became `sum(1, 7, ...)` and `var[1.7]` became `var[2]`.
#[must_use]
pub fn to_index(value: f64) -> Option<i64> {
    /// `2^53` — the largest magnitude at which every integer is representable.
    const LIMIT: f64 = 9_007_199_254_740_992.0;

    (value.is_finite() && value.fract() == 0.0 && value.abs() <= LIMIT).then_some(value as i64)
}

/// Whether `expr` is a whole number *by construction* — the crate's one type
/// judgement, and the reason a subscript needs no rounding check at run time.
///
/// `env[slot]` says whether a local is integral: an aggregate's parameter is,
/// and a `var a = …` is when its value was; the caller's walk marks them as it
/// enters the binding. The forms:
///
/// | form                                 | integral when |
/// | ------------------------------------ | ----------------------------------------------------------|
/// | literal                              | [`to_index`] accepts it |
/// | local                                | `env[slot]` |
/// | global, `var[…]`                     | never — a point's value |
/// | `floor`, `ceil`, `sgn`               | always |
/// | negation, `abs`, `sqr`, `cube`       | the argument is |
/// | `+`, `-`, `*`, `%`, `max`, `min`     | both operands are |
/// | `^`                                  | the base is and the exponent is a whole literal, `0..=64`: repeated multiplication, which is what the backends make of it |
/// | `/`, any other `^`                   | never |
/// | `sum`/`prod`, unrolled or not        | the body (every term) is, the parameter counting as integral |
/// | a block                              | its result, with its assignments marked as they go |
/// | anything boolean, any other function | never |
///
/// Every form that passes is exact in `f64`: `floor`, `ceil` and `sgn`
/// produce integers by definition, and the arithmetic in the table is
/// correctly rounded, so an integer result below 2^53 — which is
/// representable — *is* the result; a whole power is a chain of such
/// multiplications (`rewrite::unroll_powers`). Division and real powers are
/// excluded for exactly the reason `1/3` gives. So a value this accepts round-trips through
/// [`to_index`] without a rounding step anywhere, and [`to_index`]'s one
/// remaining refusal is magnitude.
///
/// Deliberately a table and not an analysis: extending it is a decision about
/// the language, taken here, once.
///
/// A function over the tree, recomputed on every call, and not a flag on the
/// node: every rewrite that builds or moves a node would otherwise have to
/// keep the flag true, and a stale `true` here would put a non-integer on a
/// gather with no runtime check behind it. Asked once per subscript at parse
/// (`rewrite::check_subscripts`) and nowhere hot; a caller with a loop asks
/// once and keeps the answer.
#[must_use]
pub(crate) fn is_integral(expr: &Expr, env: &[bool]) -> bool {
    match &expr.kind {
        Kind::Literal(value) => to_index(*value).is_some(),
        Kind::Local(slot) => env[slot.index()],
        Kind::Global(_) | Kind::DynamicIndex(_) => false,
        Kind::Unary { op, arg } => match op {
            UnaryOp::Floor | UnaryOp::Ceil | UnaryOp::Sgn => true,
            UnaryOp::Negate | UnaryOp::Abs | UnaryOp::Sqr | UnaryOp::Cube => is_integral(arg, env),
            _ => false,
        },
        Kind::Binary { op, lhs, rhs } => match op {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Rem
            | BinaryOp::Max
            | BinaryOp::Min => is_integral(lhs, env) && is_integral(rhs, env),
            BinaryOp::Pow => is_integral(lhs, env) && rhs.whole_exponent().is_some_and(|n| n >= 0),
            BinaryOp::Div | BinaryOp::LogB => false,
        },
        Kind::Aggregate { param, body, .. } => {
            let mut env = env.to_vec();
            env[param.index()] = true;
            block_is_integral(body, &mut env)
        }
        Kind::Fold { terms, .. } => terms.iter().all(|term| is_integral(term, env)),
        Kind::Block(block) => block_is_integral(block, &mut env.to_vec()),
        Kind::Compare { .. } | Kind::NearEq { .. } | Kind::And { .. } => false,
    }
}

/// [`is_integral`] over a block: each assignment marks its slot for the
/// expressions after it, and the result decides.
pub(crate) fn block_is_integral(block: &Block, env: &mut [bool]) -> bool {
    for assignment in &block.assignments {
        env[assignment.slot.index()] = is_integral(&assignment.value, env);
    }
    is_integral(&block.result, env)
}

/// The largest whole exponent a backend lowers into repeated multiplication.
///
/// Past this a chain of multiplications is the wrong shape for a solver as
/// much as for the evaluator, and `powf` takes over.
pub(crate) const POWER_LIMIT: i64 = 64;

impl Expr {
    /// The whole exponent this node spells, if it is a literal whole number
    /// within [`POWER_LIMIT`]; `None` for anything a backend hands to `powf`.
    ///
    /// One rule shared by the tape, the WGSL kernel and interval narrowing,
    /// so the three agree on which powers are polynomials without a pass
    /// enforcing it. A whole power is sign-safe everywhere and has a root to
    /// narrow through; a real one is `exp(n * ln x)`, undefined for a
    /// negative base and without an inverse anything here will use.
    #[must_use]
    pub(crate) fn whole_exponent(&self) -> Option<i64> {
        match self.kind {
            Kind::Literal(value) => to_index(value).filter(|n| n.abs() <= POWER_LIMIT),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AggregateKind, Expr, GlobalId, Kind, LocalSlot, is_integral};
    use crate::diagnostics::Span;

    fn integral(source: &str) -> bool {
        let ast = crate::parse(source).expect("the fixture parses");
        let env = vec![false; ast.program().frame_size as usize];
        is_integral(&ast.program().body.result, &env)
    }

    /// One row of the table each, for the rows the compile-time tests do not
    /// reach on their own.
    #[test]
    fn the_integral_forms_are_the_table() {
        assert!(integral("floor(x1) + ceil(x2) * 3"));
        assert!(integral("abs(-floor(x1)) % 2"));
        assert!(integral("max(floor(x1), min(2, sgn(x2)))"));
        assert!(integral("sqr(floor(x1))"));
        assert!(!integral("x1"), "a point's value");
        assert!(!integral("floor(x1) / 2"), "division is not exact");
        assert!(integral("floor(x1) ^ 2"), "a whole power is multiplication");
        assert!(!integral("floor(x1) ^ 0.5"), "a real power is not");
        assert!(
            !integral("2 ^ floor(x1)"),
            "nor an exponent the row decides"
        );
        assert!(!integral("sqrt(x1)"), "a function outside the table");
        assert!(!integral("2.5"));
    }

    fn kinds(source: &str) -> Vec<&'static str> {
        let ast = crate::parse(source).expect("the fixture parses");
        ast.program()
            .body
            .iter_preorder()
            .map(|expr| match &expr.kind {
                Kind::Literal(_) => "literal",
                Kind::Global(_) => "global",
                Kind::Local(_) => "local",
                Kind::DynamicIndex(_) => "subscript",
                Kind::Unary { .. } => "unary",
                Kind::Binary { .. } => "binary",
                Kind::Aggregate { .. } => "aggregate",
                Kind::Fold { .. } => "fold",
                Kind::Block(_) => "block",
                Kind::Compare { .. } => "compare",
                Kind::NearEq { .. } => "near-eq",
                Kind::And { .. } => "and",
            })
            .collect()
    }

    #[test]
    fn preorder_is_parents_first_left_to_right() {
        assert_eq!(
            kinds("x1 * (x2 + 3)"),
            ["binary", "global", "binary", "global", "literal"]
        );
    }

    /// Bindings and aggregates are not leaves to a query: the walk reaches
    /// the value a `var` was given and the body a `sum` unrolled into.
    #[test]
    fn preorder_reaches_under_bindings_and_aggregates() {
        let seen = kinds("sum(1, 2, i -> var a = i + 1; a * x1)");
        // Unrolled: a fold of two blocks, each `var a = <i> + 1; a * x1`.
        assert_eq!(seen[0], "fold");
        assert_eq!(seen.iter().filter(|k| **k == "block").count(), 2);
        assert_eq!(seen.iter().filter(|k| **k == "global").count(), 2);
        assert_eq!(seen.iter().filter(|k| **k == "local").count(), 2);
        assert!(seen.contains(&"literal"));
    }

    /// Every kind a parse can leave in the tree, counted against the hand
    /// count, so a variant `children` forgot would show as a short walk. An
    /// aggregate never survives a parse (its bounds must be constant, so it
    /// unrolls) and `And` is a rewrite's product, so those two are built by
    /// hand and their children counted directly.
    #[test]
    fn every_kind_yields_its_children() {
        let source = "var a = x1 + var[floor(x2)]; abs(a) < x3 * 2 - a";
        let ast = crate::parse(source).expect("the fixture parses");
        // the assignment: binary[global, subscript[unary[global]]] = 5
        // the result: compare[unary[local], binary[binary[global, literal], local]] = 8
        assert_eq!(ast.program().body.iter_preorder().count(), 13);

        let at = Span::new(0, 1);
        let leaf = || Expr::new(Kind::Literal(1.0), at);
        let block = || {
            Box::new(super::Block {
                assignments: vec![super::Assignment {
                    slot: LocalSlot::from_index(0),
                    value: leaf(),
                    span: at,
                }],
                result: leaf(),
            })
        };
        let aggregate = Expr::new(
            Kind::Aggregate {
                kind: AggregateKind::Sum,
                lower: Box::new(leaf()),
                upper: Box::new(leaf()),
                param: LocalSlot::from_index(1),
                body: block(),
            },
            at,
        );
        assert_eq!(
            aggregate.children().len(),
            4,
            "lower, upper, one assignment, result"
        );
        let and = Expr::new(
            Kind::And {
                terms: vec![leaf(), leaf(), leaf()],
            },
            at,
        );
        assert_eq!(and.children().len(), 3);
        assert_eq!(Expr::new(Kind::Block(block()), at).children().len(), 2);
    }

    /// A `var` bound to something integral is integral after it; an
    /// aggregate's parameter is integral inside it.
    #[test]
    fn locals_are_integral_when_what_bound_them_was() {
        let ast = crate::parse("var a = floor(x1); var b = a / 2; a + 1").expect("parses");
        let mut env = vec![false; ast.program().frame_size as usize];
        assert!(super::block_is_integral(&ast.program().body, &mut env));
        assert_eq!(env, vec![true, false]);
        assert!(integral("sum(1, 3, i -> 2 * i - 1)"));
        assert!(!integral("sum(1, 3, i -> i / 2)"));
    }

    #[test]
    fn a_whole_exponent_is_a_literal_integer_within_the_cap() {
        let exponent =
            |value: f64| Expr::new(Kind::Literal(value), Span::new(0, 1)).whole_exponent();
        assert_eq!(exponent(2.0), Some(2));
        assert_eq!(exponent(0.0), Some(0));
        assert_eq!(exponent(-3.0), Some(-3));
        assert_eq!(exponent(64.0), Some(64), "the cap is inclusive");
        assert_eq!(exponent(65.0), None);
        assert_eq!(exponent(2.5), None);
        assert_eq!(exponent(f64::NAN), None);

        let variable = Expr::new(Kind::Global(GlobalId::from_index(0)), Span::new(0, 1));
        assert_eq!(variable.whole_exponent(), None);
    }
}

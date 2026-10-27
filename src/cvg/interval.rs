//! Interval arithmetic: the image of each operator over a box, and what each
//! operand must be for the image to land somewhere.
//!
//! # What it is for
//!
//! The walker's central question is *conditional*: with every other coordinate
//! held where it is, what values may this one take? Answering it needs an
//! expression evaluated over a set rather than a point, which is what this is.
//! The evaluation itself — forward over a constraint, then backward through
//! these inverses — is HC4-revise over the constraint's tape, in
//! [`hc4`](super::hc4), with [`hc4::slice`](super::hc4::slice) asking it
//! across every constraint naming a coordinate. This module is the arithmetic
//! it runs.
//!
//! # The only invariant that matters
//!
//! **Every interval here is a superset of the values it models.** Not a tight
//! enclosure, a superset.
//!
//! That asymmetry is the whole design. A proposal drawn from a superset and then
//! judged by `ConstraintSystem::is_feasible` is, conditioned on acceptance, distributed
//! exactly as one drawn from the true set: uniform on `S`, restricted to
//! `T` inside `S`, is uniform on `T`. So an interval that is too wide costs a
//! rejected proposal and nothing else, while one that is too narrow removes
//! reachable points and biases the answer silently.
//!
//! Everything follows from that. Anything unknown answers [`Interval::ENTIRE`],
//! which is why this is total and has no error type: "I cannot narrow this"
//! degrades to the behaviour the walker already had.
//!
//! # Why this is not `inari`
//!
//! IEEE 1788 libraries buy *tightness together with* soundness, at the cost of
//! rounding-mode control and a dependency in a crate Artemis links. We need only
//! the soundness, and a padding of a few ulps outward buys it outright — see
//! [`Interval::rounded`] for how much padding each class of operator earns.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use crate::ast::{BinaryOp, UnaryOp};

/// A closed interval of reals, represented by two `f64` endpoints.
///
/// Empty is `lo > hi`, so an intersection that finds nothing needs no separate
/// variant and no `Option`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Interval {
    lo: f64,
    hi: f64,
}

impl Interval {
    /// Everything. The answer whenever nothing can be concluded.
    pub(crate) const ENTIRE: Self = Self {
        lo: f64::NEG_INFINITY,
        hi: f64::INFINITY,
    };

    /// Nothing. The result of an intersection with no overlap.
    pub(crate) const EMPTY: Self = Self {
        lo: f64::INFINITY,
        hi: f64::NEG_INFINITY,
    };

    /// The interval `[lo, hi]`, or [`ENTIRE`](Self::ENTIRE) if either endpoint
    /// is `NaN`.
    ///
    /// A `NaN` endpoint means an operation could not say anything, and the
    /// superset rule makes "everything" the only honest answer. It is never
    /// [`EMPTY`](Self::EMPTY): empty claims the values are unreachable, which is
    /// the one thing an unknown result must not claim.
    pub(crate) fn new(lo: f64, hi: f64) -> Self {
        if lo.is_nan() || hi.is_nan() {
            Self::ENTIRE
        } else {
            Self { lo, hi }
        }
    }

    /// The degenerate interval `[value, value]`, which is how a fixed
    /// coordinate enters a propagation.
    pub(crate) fn point(value: f64) -> Self {
        Self::new(value, value)
    }

    pub(crate) const fn lo(self) -> f64 {
        self.lo
    }

    pub(crate) const fn hi(self) -> f64 {
        self.hi
    }

    /// Empty is `lo > hi`, and that is the whole test because an endpoint is
    /// never `NaN`: [`new`](Self::new) turns one into [`ENTIRE`](Self::ENTIRE),
    /// the constants are finite or infinite, and `widened` only moves an
    /// endpoint by ulps. The fields are private to this module, so that is a
    /// discipline the compiler does not enforce; this is the function that
    /// relies on it — a `NaN` would read as non-empty and reach `width`, then
    /// `random_range`, which panics on a `NaN` bound — so it asserts it, in
    /// every build. Two comparisons on the hottest path in the crate,
    /// measured as nothing.
    pub(crate) fn is_empty(self) -> bool {
        assert!(
            !self.lo.is_nan() && !self.hi.is_nan(),
            "an interval endpoint is never NaN: {self:?}"
        );
        self.lo > self.hi
    }

    /// The width, or zero when empty. Infinite for an unbounded interval.
    pub(crate) fn width(self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.hi - self.lo
        }
    }

    pub(crate) fn contains(self, value: f64) -> bool {
        self.lo <= value && value <= self.hi
    }

    pub(crate) fn intersect(self, other: Self) -> Self {
        // Constructed directly rather than through `new`, which would turn an
        // empty result into `ENTIRE`. Here `lo > hi` is a real conclusion.
        Self {
            lo: self.lo.max(other.lo),
            hi: self.hi.min(other.hi),
        }
    }

    /// The smallest interval containing both. A union is not representable, so
    /// this is what a non-monotone inverse answers with.
    pub(crate) fn hull(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self::new(self.lo.min(other.lo), self.hi.max(other.hi))
    }

    /// Widened by `ulps` in each direction.
    ///
    /// An infinity is its own neighbour, so an unbounded interval is unchanged
    /// and this never has to be guarded.
    fn widened(self, ulps: u32) -> Self {
        let mut result = self;
        for _ in 0..ulps {
            result.lo = result.lo.next_down();
            result.hi = result.hi.next_up();
        }
        // The one place an endpoint is written outside `new`, so the one
        // place the invariant is re-established rather than relied on.
        assert!(
            !result.lo.is_nan() && !result.hi.is_nan(),
            "widening kept an endpoint finite or infinite: {result:?}"
        );
        result
    }

    /// For an operator IEEE 754 requires to be correctly rounded — the four
    /// arithmetic operations and `sqrt`. A computed endpoint is within half an
    /// ulp of the true one, so one ulp of padding covers it.
    fn rounded(lo: f64, hi: f64) -> Self {
        Self::new(lo, hi).widened(1)
    }

    /// For an operator that goes through libm, where accuracy is a quality of
    /// implementation rather than a guarantee. Two ulps covers every function
    /// Rust documents on `f64`, and covers the fact that such an implementation
    /// need not be *monotone* — which is what makes "evaluate at both
    /// endpoints" sound for a monotone function in the first place.
    fn approximated(lo: f64, hi: f64) -> Self {
        Self::new(lo, hi).widened(2)
    }

    /// For an operator that is exact in `f64`: negation, absolute value, the
    /// two rounding functions, `min`, `max` and `sgn`. Padding these would only
    /// throw width away.
    fn exact(lo: f64, hi: f64) -> Self {
        Self::new(lo, hi)
    }
}

// ------------------------------------------------------------------- forward

/// Applies a binary operator to two intervals.
pub(crate) fn binary(op: BinaryOp, a: Interval, b: Interval) -> Interval {
    if a.is_empty() || b.is_empty() {
        return Interval::EMPTY;
    }
    match op {
        BinaryOp::Add => Interval::rounded(a.lo + b.lo, a.hi + b.hi),
        BinaryOp::Sub => Interval::rounded(a.lo - b.hi, a.hi - b.lo),
        BinaryOp::Mul => corners(a, b, |x, y| x * y, 1),

        // The true image of a division by an interval straddling zero is two
        // unbounded rays: `1 / [-1, 1]` is everything at or beyond `1` in
        // either direction. Their hull is the line, and the hull is what is
        // representable.
        BinaryOp::Div => {
            if b.contains(0.0) {
                Interval::ENTIRE
            } else {
                corners(a, b, |x, y| x / y, 1)
            }
        }

        // `a % b` keeps the sign of the dividend and is smaller in magnitude
        // than the divisor. That is all this claims — the function is
        // discontinuous, and a bound is far easier to be sure of than an image.
        BinaryOp::Rem => {
            let magnitude = b.lo.abs().max(b.hi.abs());
            let (lo, hi) = if a.lo >= 0.0 {
                (0.0, magnitude)
            } else if a.hi <= 0.0 {
                (-magnitude, 0.0)
            } else {
                (-magnitude, magnitude)
            };
            Interval::exact(lo, hi)
        }

        BinaryOp::Pow => corners(a, b, f64::powf, 2),
        BinaryOp::Max => Interval::exact(a.lo.max(b.lo), a.hi.max(b.hi)),
        BinaryOp::Min => Interval::exact(a.lo.min(b.lo), a.hi.min(b.hi)),

        // `log(base, x) == ln(x) / ln(base)`, which is exactly how
        // `BinaryOp::apply` computes it, so the two agree by construction
        // rather than by coincidence. Note the operand order: `a` is the base.
        BinaryOp::LogB => binary(BinaryOp::Div, unary(UnaryOp::Ln, b), unary(UnaryOp::Ln, a)),
    }
}

/// The image of an operator monotone in each argument separately, as the hull
/// of its four corners.
///
/// A `NaN` corner answers [`Interval::ENTIRE`]. That happens where the operator
/// has no value there — `0 * inf`, a negative base raised to a fractional power
/// — and in every such case the true image is something this cannot describe,
/// so the superset rule leaves one answer.
fn corners(a: Interval, b: Interval, f: impl Fn(f64, f64) -> f64, ulps: u32) -> Interval {
    let values = [f(a.lo, b.lo), f(a.lo, b.hi), f(a.hi, b.lo), f(a.hi, b.hi)];
    if values.iter().any(|value| value.is_nan()) {
        return Interval::ENTIRE;
    }
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Interval::new(lo, hi).widened(ulps)
}

/// Applies a unary operator to an interval.
pub(crate) fn unary(op: UnaryOp, x: Interval) -> Interval {
    if x.is_empty() {
        return Interval::EMPTY;
    }
    match op {
        UnaryOp::Negate => Interval::exact(-x.hi, -x.lo),

        // Monotone everywhere they are defined, so the endpoints are the image
        // — after clipping to the domain, which is what makes `sqrt([-1, 4])`
        // answer `[0, 2]` rather than something containing a `NaN`.
        UnaryOp::Atan => monotone(x, f64::atan, true),
        UnaryOp::Sinh => monotone(x, f64::sinh, true),
        UnaryOp::Tanh => monotone(x, f64::tanh, true),
        UnaryOp::Cbrt => monotone(x, f64::cbrt, true),
        UnaryOp::Cube => monotone(x, |v| v * v * v, true),
        UnaryOp::Sqrt => monotone(x.intersect(NONNEGATIVE), f64::sqrt, true),
        UnaryOp::Ln => monotone(x.intersect(POSITIVE), f64::ln, true),
        UnaryOp::Log10 => monotone(x.intersect(POSITIVE), f64::log10, true),
        UnaryOp::Asin => monotone(x.intersect(UNIT), f64::asin, true),
        UnaryOp::Acos => monotone(x.intersect(UNIT), f64::acos, false),

        // Exact as well as monotone, so no padding is earned.
        UnaryOp::Ceil => Interval::exact(x.lo.ceil(), x.hi.ceil()),
        UnaryOp::Floor => Interval::exact(x.lo.floor(), x.hi.floor()),

        // Even, with their minimum at zero. Straddling zero is the case a
        // corners-only rule gets wrong: `sqr([-1, 2])` is `[0, 4]`, and the
        // endpoints alone say `[1, 4]`.
        UnaryOp::Abs => even(x, f64::abs, 0.0, 0),
        UnaryOp::Sqr => even(x, |v| v * v, 0.0, 1),
        UnaryOp::Cosh => even(x, f64::cosh, 1.0, 2),

        UnaryOp::Sin => sinusoid(x, f64::sin, FRAC_PI_2, -FRAC_PI_2),
        UnaryOp::Cos => sinusoid(x, f64::cos, 0.0, PI),

        // A pole inside the interval makes the image unbounded on both sides.
        // Away from one, both are monotone on the branch — `tan` upward, `cot`
        // downward.
        UnaryOp::Tan => {
            if crosses(x, FRAC_PI_2, PI) {
                Interval::ENTIRE
            } else {
                monotone(x, f64::tan, true)
            }
        }
        UnaryOp::Cot => {
            if crosses(x, 0.0, PI) {
                Interval::ENTIRE
            } else {
                monotone(x, |v| 1.0 / v.tan(), false)
            }
        }

        // Three values, and which of them are reachable is decided by where the
        // interval sits relative to zero. Babel's `sgn` answers the argument
        // itself at zero, which is a signed zero and inside `[0, 0]` either way.
        UnaryOp::Sgn => {
            if x.lo > 0.0 {
                Interval::exact(1.0, 1.0)
            } else if x.hi < 0.0 {
                Interval::exact(-1.0, -1.0)
            } else if x.lo == 0.0 && x.hi == 0.0 {
                Interval::exact(0.0, 0.0)
            } else {
                Interval::exact(-1.0, 1.0)
            }
        }
    }
}

/// `ln`'s domain, as the evaluator sees it: `ln(0)` is negative infinity, which
/// is a `NonFiniteValue` rather than an answer, so zero is outside.
const POSITIVE: Interval = Interval {
    lo: f64::MIN_POSITIVE,
    hi: f64::INFINITY,
};

/// `sqrt`'s domain, which keeps the inclusive floor `ln` cannot: `sqrt(0)` is
/// `0.0`, a perfectly good answer. The asymmetry is the same one
/// `rewrite::monotone` documents.
const NONNEGATIVE: Interval = Interval {
    lo: 0.0,
    hi: f64::INFINITY,
};

/// The domain of `asin` and `acos`.
const UNIT: Interval = Interval { lo: -1.0, hi: 1.0 };

/// The image of a strictly monotone function: its values at the endpoints,
/// ordered.
fn monotone(x: Interval, f: impl Fn(f64) -> f64, increasing: bool) -> Interval {
    if x.is_empty() {
        return Interval::EMPTY;
    }
    let (a, b) = (f(x.lo), f(x.hi));
    if increasing {
        Interval::approximated(a, b)
    } else {
        Interval::approximated(b, a)
    }
}

/// The image of an even function increasing away from zero, whose minimum is
/// therefore attained inside any interval containing zero.
fn even(x: Interval, f: impl Fn(f64) -> f64, at_zero: f64, ulps: u32) -> Interval {
    let (a, b) = (f(x.lo), f(x.hi));
    let hi = a.max(b);
    let lo = if x.contains(0.0) { at_zero } else { a.min(b) };
    Interval::new(lo, hi).widened(ulps)
}

/// `x ^ n` for a whole `n`, computed the way the tape computes it: a left fold
/// of multiplications from one. Each multiply is correctly rounded and so
/// monotone, and the composition is too, which is what makes evaluating the
/// endpoints sound with a single ulp of padding. `powi` is a different rounding
/// sequence and would need padding nobody has measured.
fn whole_power(x: f64, n: u32) -> f64 {
    (0..n).fold(1.0, |acc, _| acc * x)
}

/// The image of `x ^ n` for a whole `n`. Even powers are `sqr`'s shape with the
/// minimum at zero, odd ones are monotone, and a negative `n` is the reciprocal
/// of the positive one — through the division rule, so a base straddling zero
/// answers everything, which is the truth.
pub(crate) fn power(x: Interval, n: i64) -> Interval {
    let count = u32::try_from(n.unsigned_abs()).expect("bounded by POWER_LIMIT");
    let positive = if count == 0 {
        Interval::point(1.0)
    } else if count % 2 == 0 {
        even(x, |v| whole_power(v, count), 0.0, 1)
    } else {
        monotone(x, |v| whole_power(v, count), true)
    };
    if n < 0 {
        binary(BinaryOp::Div, Interval::point(1.0), positive)
    } else {
        positive
    }
}

/// The image of `sin` or `cos`: the endpoints, plus `1` or `-1` wherever the
/// interval reaches a crest or a trough.
///
/// `peak` and `trough` are the arguments at which the function attains `1` and
/// `-1`; everything else the two share.
fn sinusoid(x: Interval, f: impl Fn(f64) -> f64, peak: f64, trough: f64) -> Interval {
    // Beyond this an `f64`'s own spacing is coarser than the period, so
    // `(lo - peak) / TAU` can no longer say which crest is nearest and the
    // periodic test below stops meaning anything. The range is then the only
    // honest answer, and it is still a superset.
    const RELIABLE: f64 = 1e15;

    if x.width() >= TAU || x.lo.abs() > RELIABLE || x.hi.abs() > RELIABLE {
        return Interval::exact(-1.0, 1.0);
    }
    let (a, b) = (f(x.lo), f(x.hi));
    let lo = if crosses(x, trough, TAU) {
        -1.0
    } else {
        a.min(b)
    };
    let hi = if crosses(x, peak, TAU) { 1.0 } else { a.max(b) };
    Interval::new(lo, hi).widened(2)
}

/// Whether `x` contains a point of the form `offset + k * period` for some
/// integer `k`.
///
/// A crest of `sin` and a pole of `tan` are the same question asked of
/// different constants, which is why this is one function.
fn crosses(x: Interval, offset: f64, period: f64) -> bool {
    if !x.lo.is_finite() || !x.hi.is_finite() {
        return true;
    }
    let first = offset + ((x.lo - offset) / period).ceil() * period;
    first <= x.hi
}

// -------------------------------------------------------- over an expression

/// Whether [`invert_binary`] has an inverse for `op`, rather than declining.
///
/// `classify::reaches` asks this to decide whether an equality *determines* a
/// variable. The two must agree: claiming a coordinate is driven and then
/// handing the walker its whole box for it is the one combination that stalls,
/// rather than merely wasting a proposal.
pub(crate) const fn invertible_binary(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
    )
}

/// Whether [`invert_unary`] has an inverse for `op`, rather than declining.
///
/// The periodic and the discontinuous decline; everything else has one. Note
/// what this includes that a *symbolic* inverse table could not: `abs`, `sqr`
/// and `cosh` are not injective, and inverting them symbolically would have to
/// pick a branch and be silently wrong half the time. Narrowing does not pick —
/// it intersects both branches with what the argument can already be, and the
/// argument's own range settles it. That is why `reaches` may allow these
/// where `isolate`, which built an expression, could not.
pub(crate) const fn invertible_unary(op: UnaryOp) -> bool {
    !matches!(
        op,
        UnaryOp::Sin | UnaryOp::Cos | UnaryOp::Tan | UnaryOp::Cot | UnaryOp::Sgn
    )
}

/// What each operand must be for `a op b` to land in `target`.
///
/// The seven arithmetic rules of `classify::isolate`, as intervals. Anything
/// without an entry answers [`Interval::ENTIRE`] twice, which narrows nothing
/// and is always safe.
pub(crate) fn invert_binary(
    op: BinaryOp,
    target: Interval,
    a: Interval,
    b: Interval,
) -> (Interval, Interval) {
    match op {
        // `a + b in T` means `a in T - b` and `b in T - a`.
        BinaryOp::Add => (
            binary(BinaryOp::Sub, target, b),
            binary(BinaryOp::Sub, target, a),
        ),
        // `a - b in T` means `a in T + b` and `b in a - T`. The second is the
        // non-commuting one, and swapping it is the mistake that has no
        // compile error.
        BinaryOp::Sub => (
            binary(BinaryOp::Add, target, b),
            binary(BinaryOp::Sub, a, target),
        ),
        // Division by an interval straddling zero answers `ENTIRE`, so the
        // `x1 * x2 == 0` cross narrows nothing when standing on the other arm
        // — which is correct, and why it is a connectivity problem rather than
        // a narrowing one.
        BinaryOp::Mul => (
            binary(BinaryOp::Div, target, b),
            binary(BinaryOp::Div, target, a),
        ),
        // `a / b in T` means `a in T * b` and `b in a / T`, the other
        // non-commuting arm.
        BinaryOp::Div => (
            binary(BinaryOp::Mul, target, b),
            binary(BinaryOp::Div, a, target),
        ),
        // `^` with a real or variable exponent, `%`, `max`, `min` and
        // `log(base, x)` have no inverse here. The first three for the reasons
        // `classify::isolate` gives; `log` because it would want its base held
        // apart from one, which is more bookkeeping than the narrowing is
        // worth until something needs it. A whole exponent never arrives:
        // the narrower inverts it through its root before consulting this table.
        _ => (Interval::ENTIRE, Interval::ENTIRE),
    }
}

/// What the argument must be for `f(arg)` to land in `target`.
///
/// `current` is what the argument can already be, which is what makes the
/// non-injective rows possible: `sqr(u) in [4, 9]` says `u` is in `[-3, -2]` or
/// `[2, 3]`, and intersecting the hull of those with a `current` on one side of
/// zero recovers the branch without representing a union.
pub(crate) fn invert_unary(op: UnaryOp, target: Interval, current: Interval) -> Interval {
    match op {
        UnaryOp::Negate => Interval::exact(-target.hi, -target.lo),

        // Strictly monotone, so the inverse of the target's endpoints is the
        // target's preimage. These are `rewrite::monotone`'s rows, and a test
        // holds the two tables to the same answers.
        UnaryOp::Ln => monotone(target, f64::exp, true),
        UnaryOp::Log10 => monotone(target, |v| 10.0_f64.powf(v), true),
        UnaryOp::Sqrt => monotone(target.intersect(NONNEGATIVE), |v| v * v, true),
        UnaryOp::Cbrt => monotone(target, |v| v * v * v, true),
        UnaryOp::Cube => monotone(target, f64::cbrt, true),
        UnaryOp::Sinh => monotone(target, f64::asinh, true),
        UnaryOp::Tanh => monotone(target.intersect(UNIT), f64::atanh, true),
        UnaryOp::Atan => monotone(target.intersect(HALF_TURN), f64::tan, true),
        UnaryOp::Asin => monotone(target, f64::sin, true),
        UnaryOp::Acos => monotone(target, f64::cos, false),

        // Even, so the preimage is two branches. The hull of them is what is
        // representable, and `current` is what picks one back out.
        UnaryOp::Abs => symmetric(
            monotone(target.intersect(NONNEGATIVE), |v| v, true),
            current,
        ),
        UnaryOp::Sqr => symmetric(
            monotone(target.intersect(NONNEGATIVE), f64::sqrt, true),
            current,
        ),
        UnaryOp::Cosh => symmetric(
            monotone(target.intersect(above_one()), f64::acosh, true),
            current,
        ),

        // `floor(u) in [a, b]` means `u < b + 1`, and `ceil(u) in [a, b]` means
        // `u > a - 1`. Cheap, exact enough, and it keeps a rounded coordinate
        // from being unnarrowable.
        UnaryOp::Floor => Interval::exact(target.lo, target.hi + 1.0),
        UnaryOp::Ceil => Interval::exact(target.lo - 1.0, target.hi),

        // The periodic and the discontinuous. `sin(u) in [0.4, 0.5]` is a
        // countable union of intervals whose hull, over an unbounded argument,
        // is everything — so narrowing needs to know which branch `current`
        // sits on, and that is a piece of work in its own right rather than a
        // row in a table. Forward propagation through `sin` already works and
        // is the direction `y == sin(x)` actually needs.
        UnaryOp::Sin | UnaryOp::Cos | UnaryOp::Tan | UnaryOp::Cot | UnaryOp::Sgn => {
            Interval::ENTIRE
        }
    }
}

/// `atan`'s range, which is also the only target interval its inverse accepts:
/// `tan` of anything outside it is a different branch.
const HALF_TURN: Interval = Interval {
    lo: -FRAC_PI_2,
    hi: FRAC_PI_2,
};

fn above_one() -> Interval {
    Interval::new(1.0, f64::INFINITY)
}

/// The preimage of an even function, given `positive`, its preimage on the
/// non-negative side: `+/- positive`, as a hull.
fn symmetric(positive: Interval, current: Interval) -> Interval {
    if positive.is_empty() {
        return Interval::EMPTY;
    }
    let negative = Interval::exact(-positive.hi(), -positive.lo());
    // Both branches are candidates, but `current` usually rules one out — which
    // is what turns an unusable hull back into a narrowing.
    let live = |branch: Interval| !branch.intersect(current).is_empty();
    match (live(positive), live(negative)) {
        (true, false) => positive,
        (false, true) => negative,
        _ => positive.hull(negative),
    }
}

/// What the base must be for `base ^ n` to land in `target`, for a whole `n`.
///
/// An even power is `sqr`'s rule with an nth root, the base's own range
/// picking the branch; an odd one is monotone through a signed root. A
/// negative `n` is inverted through the reciprocal first, after which a target
/// straddling zero concludes nothing — which is the truth, since the base can
/// then be anything large.
pub(crate) fn invert_power(target: Interval, n: i64, current: Interval) -> Interval {
    let count = u32::try_from(n.unsigned_abs()).expect("bounded by POWER_LIMIT");
    if count == 0 {
        return Interval::ENTIRE;
    }
    // `1 / p in target` means `p in 1 / target`.
    let target = if n < 0 {
        binary(BinaryOp::Div, Interval::point(1.0), target)
    } else {
        target
    };
    if count % 2 == 0 {
        symmetric(
            root_enclosure(target.intersect(NONNEGATIVE), count),
            current,
        )
    } else {
        root_enclosure(target, count)
    }
}

/// The values whose `n`th power lands in `t`: an interval each of whose
/// endpoints is the tape's own multiplication brackets `t`, so it encloses the
/// preimage however libm rounds the root. For an even `n` the caller passes a
/// non-negative `t` and gets the non-negative branch.
fn root_enclosure(t: Interval, n: u32) -> Interval {
    if t.is_empty() {
        return Interval::EMPTY;
    }
    Interval::new(bracket(t.lo, n, true), bracket(t.hi, n, false))
}

/// A root of `t` nudged outward — `down` for a lower endpoint — until
/// [`whole_power`] of it lands on the right side of `t`.
///
/// `sqrt` is correctly rounded and `cbrt` nearly so, but `powf(t, 1.0 / n)`
/// carries the rounding of `1.0 / n` amplified by `ln t`, which is tens of
/// ulps for a large `t`. Nudging one ulp at a time against the power the tape
/// actually computes makes the enclosure exact by construction, and the power
/// is monotone so the walk is short and always ends.
fn bracket(t: f64, n: u32, down: bool) -> f64 {
    const NUDGES: u32 = 4_096;

    let magnitude = match n {
        1 => t.abs(),
        2 => t.abs().sqrt(),
        3 => t.abs().cbrt(),
        _ => t.abs().powf(1.0 / f64::from(n)),
    };
    let mut root = t.signum() * magnitude;
    for _ in 0..NUDGES {
        let landed = whole_power(root, n);
        if (down && landed <= t) || (!down && landed >= t) {
            return root;
        }
        root = if down {
            root.next_down()
        } else {
            root.next_up()
        };
    }
    if down {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    }
}

#[cfg(test)]
mod tests {
    use rand::rngs::Xoshiro256PlusPlus;
    use rand::{RngExt, SeedableRng};

    use crate::ast::GlobalId;

    use super::Interval;
    use crate::cvg::hc4::{self, Frames, slice};
    use crate::eval;
    use crate::system::tests::system;
    use crate::{Ast, InputVariable, Point};

    /// **The invariant `slice` exists to keep.**
    ///
    /// A feasible point satisfies every constraint, so each of its coordinates
    /// is in that coordinate's true feasible slice — and the narrowing is a
    /// *superset* of that slice, so it must contain the coordinate too. If it
    /// ever does not, the walker is being handed an interval that excludes
    /// where it is standing, and the points it cannot propose are gone from the
    /// answer silently.
    ///
    /// This is the check that separates the two ways narrowing can be wrong.
    /// Too wide only costs a rejected proposal; too narrow biases, and shows up
    /// here.
    #[test]
    fn a_feasible_point_is_inside_every_slice_it_sits_in() {
        let fixtures: [(Vec<InputVariable>, &[&str]); 5] = [
            (
                vec![
                    InputVariable::new_continuous("x1", -10.0, 10.0),
                    InputVariable::new_continuous("x2", -10.0, 10.0),
                ],
                &["x1 + x2 == 3 +/- 0.1"],
            ),
            (
                vec![
                    InputVariable::new_continuous("x1", -10.0, 10.0),
                    InputVariable::new_continuous("x2", -10.0, 10.0),
                ],
                &["x1 + x2 < 3", "x1 - x2 > -4"],
            ),
            (
                vec![
                    InputVariable::new_continuous("x1", 0.5, 10.0),
                    InputVariable::new_continuous("x2", 0.5, 10.0),
                ],
                &["x1 / x2 < 4", "ln(x1) < 2"],
            ),
            (
                vec![
                    InputVariable::new_continuous("x1", -10.0, 10.0),
                    InputVariable::new_continuous("x2", -10.0, 10.0),
                ],
                &["x1 * x2 == 6 +/- 0.5"],
            ),
            (
                vec![InputVariable::new_continuous("x1", 10.0, 11.0)],
                &["x1 > 10.5"],
            ),
        ];

        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x0051_1CE5);
        for (inputs, sources) in fixtures {
            let bounds: Vec<(f64, f64)> = inputs
                .iter()
                .map(|input| (input.lower(), input.upper()))
                .collect();
            let system = system(inputs, sources);

            let mut feasible_seen = 0_usize;
            for _ in 0..20_000 {
                let point: Point = bounds
                    .iter()
                    .map(|(low, high)| rng.random_range(*low..=*high))
                    .collect();
                if !system.is_feasible(&point, 0.0) {
                    continue;
                }
                feasible_seen += 1;
                for coordinate in 0..point.len() {
                    let slice = slice(&system, &point, coordinate);
                    assert!(
                        slice.contains(point[coordinate]),
                        "{sources:?}: the feasible point {point:?} has coordinate \
                         {coordinate} narrowed out of [{}, {}]",
                        slice.lo(),
                        slice.hi()
                    );
                }
            }
            assert!(
                feasible_seen > 0,
                "{sources:?}: no feasible point was drawn, so this asserted nothing"
            );
        }
    }

    /// **`slice` can only tighten what the equality already said.**
    ///
    /// `retract` used to draw from `f(free) ± t`, evaluated from the one
    /// equality that defined the coordinate. It now draws from `slice`, which
    /// starts at the declared box and applies every constraint naming that
    /// coordinate — the defining equality among them. So the new interval is
    /// contained in the old one, and the change is a narrowing rather than a
    /// different claim.
    ///
    /// The band is computed here from **Rust's own `sin`** rather than read off
    /// a `Drive`. That is the point: nothing in production carries the
    /// definition any more, so a test that asked production what the band was
    /// would be asking the thing under test. If this ever fails, `slice` is
    /// admitting values the equality forbids.
    const TOLERANCE: f64 = 0.05;

    #[test]
    fn a_slice_is_never_wider_than_the_band_its_equality_defines() {
        let inputs = vec![
            InputVariable::new_continuous("x", -4.0, 4.0),
            InputVariable::new_continuous("y", -8.0, 8.0),
            InputVariable::new_continuous("z", -8.0, 8.0),
        ];
        let bounds: Vec<(f64, f64)> = inputs
            .iter()
            .map(|input| (input.lower(), input.upper()))
            .collect();
        let system = system(inputs, &["y == sin(x) +/- 0.05", "z == y + 1 +/- 0.05"]);

        /// A driven coordinate and what its equality says its centre is.
        /// `x` is free and has no band, so it is absent.
        type Band = (usize, fn(&Point) -> f64);

        let bands: [Band; 2] = [(1, |point| point[0].sin()), (2, |point| point[1] + 1.0)];

        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x0000_BA4D);
        for _ in 0..5_000 {
            let point: Point = bounds
                .iter()
                .map(|(low, high)| rng.random_range(*low..=*high))
                .collect();

            for (position, centre_of) in bands {
                let centre = centre_of(&point);
                let slice = slice(&system, &point, position);
                if slice.is_empty() {
                    continue;
                }
                // Narrowing pads outward once per operation it passes through,
                // so the interval exceeds the band by a rounding step per node.
                // The slack is *relative* rather than counted in ulps, because
                // an ulp is not a stable unit across a subtraction that
                // cancels: the padding is added while the intermediate is
                // around `centre`, and the endpoint it lands on can be an order
                // of magnitude smaller, where ulps are an order of magnitude
                // finer. Measured in ulps of the endpoint, two ulps of real
                // padding read as nine.
                let slack = 16.0 * f64::EPSILON * (1.0 + centre.abs() + TOLERANCE);
                assert!(
                    slice.lo() >= centre - TOLERANCE - slack
                        && slice.hi() <= centre + TOLERANCE + slack,
                    "coordinate {position} narrowed to [{}, {}], outside the \
                     band {centre} +/- {TOLERANCE} its equality defines",
                    slice.lo(),
                    slice.hi()
                );
            }
        }
    }

    /// The interval an expression takes over a box.
    ///
    /// `globals` is indexed by [`GlobalId`](crate::ast::GlobalId) — the expression's
    /// own symbol list rather than the schema, which is the indexing `classify`
    /// already uses and the one `Ast::bind` maps onto row positions.
    ///
    /// Total, like everything else here: a node it cannot read answers
    /// [`Interval::ENTIRE`].
    ///
    /// The narrower's forward sweep with nothing to invert, which is what the
    /// containment property is stated against.
    fn evaluate(ast: &Ast, globals: &[Interval]) -> Interval {
        hc4::compile(ast).evaluate(globals, &mut Frames::default())
    }

    /// Points drawn per expression. Enough that a wrong sign or a missed
    /// extremum shows up; small enough that the whole module stays instant.
    const DRAWS: usize = 2_000;

    /// **The load-bearing check of this module.**
    ///
    /// Compiles `source`, computes its interval over the box `ranges`, then
    /// draws points from that box and requires the evaluator's answer at each
    /// to lie inside it.
    ///
    /// The evaluator is the oracle, so nothing here has to predict a number —
    /// which matters because the numbers are the part a reader cannot check.
    /// What is being asserted is the one property the walker depends on:
    /// **the interval is a superset**. A too-wide interval passes, and should:
    /// it costs a rejected proposal and never a wrong point.
    ///
    /// A non-finite result is skipped rather than asserted on. `eval_one`
    /// reports one as an error and the pool discards such a point, so it is not
    /// a value the interval has to contain.
    fn assert_contains(source: &str, ranges: &[(f64, f64)]) {
        let ast = crate::parse(source).expect("the source should compile");
        assert_eq!(
            ast.symbols().len(),
            ranges.len(),
            "{source}: one range per symbol, in `symbols` order"
        );

        let globals: Vec<Interval> = ranges
            .iter()
            .map(|(lo, hi)| Interval::new(*lo, *hi))
            .collect();
        let enclosure = evaluate(&ast, &globals);

        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1_0000_0007);
        let mut checked = 0_usize;
        for _ in 0..DRAWS {
            let drawn: Vec<f64> = ranges
                .iter()
                .map(|(lo, hi)| rng.random_range(*lo..=*hi))
                .collect();
            let bindings: Vec<(&str, f64)> = ast
                .symbols()
                .iter()
                .map(String::as_str)
                .zip(drawn.iter().copied())
                .collect();

            let Ok(value) = eval::eval_one(ast.source(), &bindings) else {
                continue;
            };
            assert!(
                enclosure.contains(value),
                "{source} at {bindings:?} evaluates to {value}, outside the \
                 enclosure [{}, {}]",
                enclosure.lo(),
                enclosure.hi()
            );
            checked += 1;
        }

        assert!(
            checked > DRAWS / 100,
            "{source}: only {checked} of {DRAWS} draws evaluated, so this \
             asserted almost nothing — widen the box or pick another"
        );
        // Containment on its own is satisfied by an implementation that
        // answers `ENTIRE` to everything, which is sound and useless. Every
        // case reaching here has a bounded image, so requiring one turns each
        // of these into a two-sided check: wide enough to be right, narrow
        // enough to be worth computing. The operators that are genuinely
        // unbounded go through `assert_contains_unbounded` instead.
        assert!(
            enclosure.lo().is_finite() && enclosure.hi().is_finite(),
            "{source}: the enclosure is unbounded, which is sound but narrows nothing"
        );
    }

    /// For the operators whose image really is unbounded, where the tightness
    /// check above would be asking for something untrue.
    fn assert_contains_unbounded(source: &str, ranges: &[(f64, f64)]) {
        let ast = crate::parse(source).expect("the source should compile");
        let globals: Vec<Interval> = ranges
            .iter()
            .map(|(lo, hi)| Interval::new(*lo, *hi))
            .collect();
        let enclosure = evaluate(&ast, &globals);

        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1_0000_0007);
        for _ in 0..DRAWS {
            let drawn: Vec<f64> = ranges
                .iter()
                .map(|(lo, hi)| rng.random_range(*lo..=*hi))
                .collect();
            let bindings: Vec<(&str, f64)> = ast
                .symbols()
                .iter()
                .map(String::as_str)
                .zip(drawn.iter().copied())
                .collect();
            if let Ok(value) = eval::eval_one(ast.source(), &bindings) {
                assert!(
                    enclosure.contains(value),
                    "{source} at {bindings:?} evaluates to {value}, outside the enclosure"
                );
            }
        }
    }

    /// A single-variable expression over a box straddling zero, which is the
    /// range most likely to catch a sign mistake.
    fn assert_contains_over_zero(source: &str) {
        assert_contains(source, &[(-3.0, 5.0)]);
    }

    /// For the functions with a domain: a box entirely inside it.
    fn assert_contains_over_positives(source: &str) {
        assert_contains(source, &[(0.25, 12.0)]);
    }

    // ------------------------------------------------- the four arithmetic ops

    #[test]
    fn addition_encloses() {
        assert_contains("x + y", &[(-3.0, 5.0), (-8.0, 2.0)]);
    }

    #[test]
    fn subtraction_encloses() {
        assert_contains("x - y", &[(-3.0, 5.0), (-8.0, 2.0)]);
    }

    /// Both operands straddle zero, so the extremes come from the *cross*
    /// corners — `lo * hi` — and an implementation that only looked at
    /// `lo * lo` and `hi * hi` would answer `[-15, 10]` where the truth
    /// reaches `-24`.
    #[test]
    fn multiplication_takes_its_extremes_from_the_cross_corners() {
        assert_contains("x * y", &[(-3.0, 5.0), (-8.0, 2.0)]);
    }

    /// A divisor away from zero is four corners like any other operator.
    #[test]
    fn division_by_a_bounded_divisor_encloses() {
        assert_contains("x / y", &[(-3.0, 5.0), (2.0, 8.0)]);
    }

    /// A divisor straddling zero: the image is two unbounded rays and the only
    /// representable superset is everything. The evaluator answers a finite
    /// number for every point that is not exactly zero, and every one of them
    /// has to be inside.
    #[test]
    fn division_by_a_divisor_straddling_zero_is_unbounded() {
        assert_contains_unbounded("x / y", &[(1.0, 2.0), (-4.0, 4.0)]);
        assert_eq!(
            super::binary(
                crate::ast::BinaryOp::Div,
                Interval::new(1.0, 2.0),
                Interval::new(-4.0, 4.0)
            ),
            Interval::ENTIRE
        );
    }

    // ----------------------------------------------------- the awkward binaries

    /// `%` is discontinuous, so this bounds rather than encloses: the result
    /// keeps the dividend's sign and is smaller than the divisor.
    #[test]
    fn remainder_is_bounded_by_the_divisor() {
        assert_contains("x % y", &[(-3.0, 5.0), (2.0, 8.0)]);
    }

    #[test]
    fn a_power_with_a_positive_base_encloses() {
        assert_contains("x ^ y", &[(0.5, 3.0), (-2.0, 2.0)]);
    }

    /// A whole power over a box straddling zero: the even ones reach their
    /// minimum at zero rather than at an endpoint, the odd ones are monotone,
    /// and a negative one is a reciprocal with a domain.
    #[test]
    fn a_whole_power_encloses_on_either_side_of_zero() {
        assert_contains_over_zero("x ^ 2");
        assert_contains_over_zero("x ^ 3");
        assert_contains_over_zero("x ^ 4");
        assert_contains_over_zero("x ^ 7");
        assert_contains_over_zero("(x + 1) ^ 2");
        assert_contains_over_positives("x ^ -1");
        assert_contains_over_positives("x ^ -2");
        assert_contains_over_positives("x ^ -3");
        assert!(
            super::power(Interval::new(-3.0, 5.0), 2).contains(0.0),
            "the minimum of an even power is at zero, not at an endpoint"
        );
    }

    #[test]
    fn max_encloses() {
        assert_contains("max(x, y)", &[(-3.0, 5.0), (-8.0, 2.0)]);
    }

    #[test]
    fn min_encloses() {
        assert_contains("min(x, y)", &[(-3.0, 5.0), (-8.0, 2.0)]);
    }

    /// `log(base, x)` is computed here the same way `BinaryOp::apply` computes
    /// it — `ln(x) / ln(base)` — so the two agree by construction.
    #[test]
    fn a_logarithm_to_a_base_encloses() {
        assert_contains("log(y, x)", &[(2.0, 8.0), (0.25, 12.0)]);
    }

    /// A base whose range spans one. `log(1, x)` is `ln(x) / ln(1)`, a division
    /// by zero, so the image really is unbounded — and this is the first thing
    /// the tightness check caught, on a fixture rather than on the code.
    #[test]
    fn a_logarithm_whose_base_could_be_one_is_unbounded() {
        assert_contains_unbounded("log(y, x)", &[(0.25, 12.0), (2.0, 8.0)]);
    }

    // --------------------------------------------------- monotone unary functions

    #[test]
    fn negation_encloses() {
        assert_contains_over_zero("-x");
    }

    #[test]
    fn atan_encloses() {
        assert_contains_over_zero("atan(x)");
    }

    #[test]
    fn sinh_encloses() {
        assert_contains_over_zero("sinh(x)");
    }

    #[test]
    fn tanh_encloses() {
        assert_contains_over_zero("tanh(x)");
    }

    #[test]
    fn cbrt_encloses() {
        assert_contains_over_zero("cbrt(x)");
    }

    #[test]
    fn cube_encloses() {
        assert_contains_over_zero("cube(x)");
    }

    #[test]
    fn ceil_encloses() {
        assert_contains_over_zero("ceil(x)");
    }

    #[test]
    fn floor_encloses() {
        assert_contains_over_zero("floor(x)");
    }

    #[test]
    fn asin_encloses() {
        assert_contains("asin(x)", &[(-1.0, 1.0)]);
    }

    /// The one decreasing row in the table, so the endpoints have to be
    /// swapped. A copy of the increasing arm would answer an inverted interval,
    /// which reads as *empty* — and empty is the one wrong answer that does not
    /// merely cost a rejection.
    #[test]
    fn acos_is_decreasing_and_still_encloses() {
        assert_contains("acos(x)", &[(-1.0, 1.0)]);
        let image = super::unary(crate::ast::UnaryOp::Acos, Interval::new(-1.0, 1.0));
        assert!(!image.is_empty(), "the endpoints were not reordered");
    }

    // ------------------------------------------------------------- the domains

    /// A box reaching below zero, where the evaluator answers `NaN` and the
    /// interval must describe only the part of the box that has answers.
    #[test]
    fn sqrt_clips_to_its_domain_rather_than_answering_nan() {
        assert_contains("sqrt(x)", &[(-4.0, 9.0)]);
        assert_eq!(
            super::unary(crate::ast::UnaryOp::Sqrt, Interval::new(-4.0, 9.0)).lo(),
            0.0_f64.next_down().next_down()
        );
    }

    #[test]
    fn ln_clips_to_its_domain() {
        assert_contains("ln(x)", &[(-4.0, 9.0)]);
    }

    #[test]
    fn log10_clips_to_its_domain() {
        assert_contains_over_positives("log(x)");
    }

    // ------------------------------------------- even functions, minimum inside

    /// `sqr([-3, 5])` is `[0, 25]`. Evaluating the endpoints alone gives
    /// `[9, 25]` and loses every point near zero — narrower than the truth, so
    /// this is the class of mistake that biases rather than the class that
    /// costs a rejection.
    #[test]
    fn sqr_reaches_zero_when_its_argument_straddles_zero() {
        assert_contains_over_zero("sqr(x)");
        assert!(
            super::unary(crate::ast::UnaryOp::Sqr, Interval::new(-3.0, 5.0)).contains(0.0),
            "the minimum of an even function is at zero, not at an endpoint"
        );
    }

    #[test]
    fn abs_reaches_zero_when_its_argument_straddles_zero() {
        assert_contains_over_zero("abs(x)");
        assert!(super::unary(crate::ast::UnaryOp::Abs, Interval::new(-3.0, 5.0)).contains(0.0));
    }

    #[test]
    fn cosh_bottoms_out_at_one() {
        assert_contains_over_zero("cosh(x)");
        assert!(super::unary(crate::ast::UnaryOp::Cosh, Interval::new(-3.0, 5.0)).contains(1.0));
    }

    #[test]
    fn sgn_encloses() {
        assert_contains_over_zero("sgn(x)");
    }

    // ------------------------------------------------------------- the periodic

    /// An interval containing a crest. `sin([1, 2])` reaches `1` at `pi/2`,
    /// which is interior — the endpoints give only `[0.84, 0.91]`.
    #[test]
    fn sin_reaches_its_crest_when_the_interval_contains_one() {
        assert_contains("sin(x)", &[(1.0, 2.0)]);
        let image = super::unary(crate::ast::UnaryOp::Sin, Interval::new(1.0, 2.0));
        assert!(
            image.hi() >= 1.0,
            "the crest at pi/2 is inside [1, 2] and was missed: {image:?}"
        );
    }

    /// The same for a trough, and on the other function.
    #[test]
    fn cos_reaches_its_trough_when_the_interval_contains_one() {
        assert_contains("cos(x)", &[(2.0, 4.0)]);
        let image = super::unary(crate::ast::UnaryOp::Cos, Interval::new(2.0, 4.0));
        assert!(
            image.lo() <= -1.0,
            "the trough at pi is inside [2, 4] and was missed: {image:?}"
        );
    }

    /// Wider than a period, so no reasoning about crests is needed or wanted.
    #[test]
    fn sin_over_more_than_a_period_is_the_whole_range() {
        assert_contains("sin(x)", &[(-10.0, 10.0)]);
        assert_eq!(
            super::unary(crate::ast::UnaryOp::Sin, Interval::new(-10.0, 10.0)),
            Interval::new(-1.0, 1.0)
        );
    }

    /// An interval so large that dividing it by the period says nothing about
    /// which crest is nearest. The range is still sound.
    #[test]
    fn sin_of_an_enormous_argument_falls_back_to_the_range() {
        assert_contains("sin(x)", &[(1e16, 1e16 + 1.0)]);
    }

    /// `tan` has a pole at `pi/2`, which `[1, 2]` contains, so the image is
    /// unbounded in both directions.
    #[test]
    fn tan_across_a_pole_is_unbounded() {
        assert_contains_unbounded("tan(x)", &[(1.0, 2.0)]);
        assert_eq!(
            super::unary(crate::ast::UnaryOp::Tan, Interval::new(1.0, 2.0)),
            Interval::ENTIRE
        );
    }

    /// Between poles it is an ordinary increasing function, and answering
    /// `ENTIRE` here would be sound but useless.
    #[test]
    fn tan_between_poles_is_bounded() {
        assert_contains("tan(x)", &[(0.1, 1.4)]);
        let image = super::unary(crate::ast::UnaryOp::Tan, Interval::new(0.1, 1.4));
        assert!(
            image.hi().is_finite(),
            "no pole lies in [0.1, 1.4] and the image should be bounded"
        );
    }

    #[test]
    fn cot_across_a_pole_is_unbounded() {
        assert_contains_unbounded("cot(x)", &[(-1.0, 1.0)]);
        assert_eq!(
            super::unary(crate::ast::UnaryOp::Cot, Interval::new(-1.0, 1.0)),
            Interval::ENTIRE
        );
    }

    #[test]
    fn cot_between_poles_is_bounded() {
        assert_contains("cot(x)", &[(0.2, 2.9)]);
    }

    // ------------------------------------------------------------ compositions

    /// Several rules at once, over a fold, which is the shape the corpus is
    /// actually made of.
    #[test]
    fn a_composition_over_a_fold_encloses() {
        assert_contains(
            "sum(1, 3, i -> sin(x * i) + y / (i + 1))",
            &[(-2.0, 2.0), (-5.0, 5.0)],
        );
    }

    /// A `let` block, so the frame is exercised rather than only the tree.
    #[test]
    fn a_block_encloses() {
        assert_contains(
            "var a = x * 2; var b = a - y; a * b",
            &[(-3.0, 3.0), (-1.0, 4.0)],
        );
    }

    // --------------------------------------------------- the type on its own

    /// **The two claims about invertibility must agree.**
    ///
    /// `classify::reaches` asks `invertible_unary` / `invertible_binary` whether
    /// an equality determines a variable; `invert_unary` / `invert_binary` are
    /// what then has to narrow it. If a predicate says yes where the table
    /// declines, the walker is told a coordinate is driven and then handed its
    /// whole box for it — which stalls, where an honest refusal only costs a
    /// wasted proposal. This is what stops the pair drifting: the predicate is
    /// a `const fn` and the table is a `match`, and nothing else connects them.
    #[test]
    fn the_invertibility_predicates_match_the_tables() {
        use crate::ast::{BinaryOp, UnaryOp};

        // A target and an argument range benign enough that a decline can only
        // mean "no inverse", never "out of range here".
        let target = Interval::new(1.5, 2.0);
        let current = Interval::new(0.5, 3.0);

        for op in [
            UnaryOp::Negate,
            UnaryOp::Cos,
            UnaryOp::Sin,
            UnaryOp::Tan,
            UnaryOp::Acos,
            UnaryOp::Asin,
            UnaryOp::Atan,
            UnaryOp::Cosh,
            UnaryOp::Sinh,
            UnaryOp::Tanh,
            UnaryOp::Cot,
            UnaryOp::Ln,
            UnaryOp::Log10,
            UnaryOp::Abs,
            UnaryOp::Sqrt,
            UnaryOp::Cbrt,
            UnaryOp::Sqr,
            UnaryOp::Cube,
            UnaryOp::Ceil,
            UnaryOp::Floor,
            UnaryOp::Sgn,
        ] {
            let narrows = super::invert_unary(op, target, current) != Interval::ENTIRE;
            assert_eq!(
                narrows,
                super::invertible_unary(op),
                "{op:?}: the predicate and the table disagree"
            );
        }

        for op in [
            BinaryOp::Add,
            BinaryOp::Sub,
            BinaryOp::Mul,
            BinaryOp::Div,
            BinaryOp::Rem,
            BinaryOp::Pow,
            BinaryOp::Max,
            BinaryOp::Min,
            BinaryOp::LogB,
        ] {
            let (a, b) = super::invert_binary(op, target, current, current);
            let narrows = a != Interval::ENTIRE || b != Interval::ENTIRE;
            assert_eq!(
                narrows,
                super::invertible_binary(op),
                "{op:?}: the predicate and the table disagree"
            );
        }
    }

    /// The soundness rule that everything else leans on. A `NaN` endpoint means
    /// "nothing could be concluded", and the superset invariant makes that
    /// *everything* — never nothing. Answering `EMPTY` would claim the values
    /// are unreachable, and the walker would stop proposing them.
    #[test]
    fn an_unknown_endpoint_becomes_everything_and_never_nothing() {
        let unknown = Interval::new(f64::NAN, 1.0);
        assert_eq!(unknown, Interval::ENTIRE);
        assert!(!unknown.is_empty());
    }

    #[test]
    fn intersecting_disjoint_intervals_is_empty() {
        let disjoint = Interval::new(0.0, 1.0).intersect(Interval::new(2.0, 3.0));
        assert!(disjoint.is_empty());
        assert_eq!(disjoint.width(), 0.0);
    }

    /// An intersection may find nothing, and that conclusion has to survive —
    /// which is why `intersect` builds the value directly instead of going
    /// through `new`, whose `NaN` rule would turn it into `ENTIRE`.
    #[test]
    fn an_empty_intersection_stays_empty_through_arithmetic() {
        let empty = Interval::new(0.0, 1.0).intersect(Interval::new(2.0, 3.0));
        assert!(super::binary(crate::ast::BinaryOp::Add, empty, Interval::point(1.0)).is_empty());
        assert!(super::unary(crate::ast::UnaryOp::Sin, empty).is_empty());
    }

    #[test]
    fn a_hull_covers_both_sides_and_ignores_an_empty_one() {
        let empty = Interval::EMPTY;
        assert_eq!(
            Interval::new(0.0, 1.0).hull(Interval::new(4.0, 5.0)),
            Interval::new(0.0, 5.0)
        );
        assert_eq!(Interval::new(0.0, 1.0).hull(empty), Interval::new(0.0, 1.0));
        assert_eq!(empty.hull(Interval::new(0.0, 1.0)), Interval::new(0.0, 1.0));
    }

    /// Padding is what buys soundness without rounding-mode control, so it must
    /// only ever grow an interval.
    #[test]
    fn padding_only_widens() {
        let exact = Interval::new(1.0, 2.0);
        let padded = exact.widened(2);
        assert!(padded.lo() < exact.lo() && padded.hi() > exact.hi());
        assert!(padded.contains(exact.lo()) && padded.contains(exact.hi()));
    }

    /// An infinity has no next value, so an unbounded interval survives padding
    /// unchanged rather than becoming a `NaN`.
    #[test]
    fn padding_an_unbounded_interval_changes_nothing() {
        assert_eq!(Interval::ENTIRE.widened(2), Interval::ENTIRE);
    }

    // ------------------------------------------------------ backward narrowing

    /// Narrows `wanted` under `source`, with the other variables pinned at
    /// `held` and `wanted` free across `range`.
    ///
    /// Names are matched against the constraint's own symbol list, which is
    /// what `GlobalId` indexes.
    fn narrowed(source: &str, wanted: &str, range: (f64, f64), held: &[(&str, f64)]) -> Interval {
        let ast = crate::parse(source).expect("the source should compile");
        let position = ast
            .symbols()
            .iter()
            .position(|name| name == wanted)
            .unwrap_or_else(|| panic!("{source} never mentions {wanted}"));

        let globals: Vec<Interval> = ast
            .symbols()
            .iter()
            .enumerate()
            .map(|(index, name)| {
                if index == position {
                    return Interval::new(range.0, range.1);
                }
                let value = held
                    .iter()
                    .find(|(held_name, _)| held_name == name)
                    .unwrap_or_else(|| panic!("{source}: nothing holds {name}"))
                    .1;
                Interval::point(value)
            })
            .collect();

        let index = u32::try_from(position).expect("fewer than four billion symbols");
        hc4::compile(&ast).narrow(
            &globals,
            GlobalId::from_index(index),
            &mut Frames::default(),
        )
    }

    /// **The soundness check for the backward pass.**
    ///
    /// Sweeps `wanted` across its range on a fine grid and requires every value
    /// that actually satisfies the constraint to be inside the narrowed
    /// interval. A narrowing that is too wide passes, as it should; one that
    /// excludes a feasible value fails, because that is the mistake that
    /// removes reachable points and biases the walk.
    ///
    /// The constraint is its own oracle again: babel evaluates a boolean to a
    /// residual that is `<= 0` exactly when it holds, so nothing here has to
    /// work out for itself where the feasible values are.
    fn assert_no_feasible_value_is_excluded(
        source: &str,
        wanted: &str,
        range: (f64, f64),
        held: &[(&str, f64)],
    ) {
        const STEPS: usize = 4_000;

        let ast = crate::parse(source).expect("the source should compile");
        let interval = narrowed(source, wanted, range, held);
        let mut feasible = 0_usize;

        for step in 0..=STEPS {
            let value = (range.1 - range.0).mul_add(step as f64 / STEPS as f64, range.0);
            let mut bindings: Vec<(&str, f64)> = held.to_vec();
            bindings.push((wanted, value));

            let Ok(residual) = eval::eval_one(ast.source(), &bindings) else {
                continue;
            };
            if residual > 0.0 {
                continue;
            }
            feasible += 1;
            assert!(
                interval.contains(value),
                "{source}: {wanted} = {value} satisfies the constraint but was \
                 narrowed out of [{}, {}]",
                interval.lo(),
                interval.hi()
            );
        }

        assert!(
            feasible > 0,
            "{source}: no value of {wanted} in {range:?} satisfies it, so this \
             asserted nothing"
        );
    }

    /// The narrowed width as a fraction of the box it started from. One means
    /// nothing was learnt.
    fn narrowing_ratio(source: &str, wanted: &str, range: (f64, f64), held: &[(&str, f64)]) -> f64 {
        narrowed(source, wanted, range, held).width() / (range.1 - range.0)
    }

    /// Nothing was concluded beyond what the caller already knew.
    ///
    /// The answer is the declared range rather than `ENTIRE`, because a
    /// backward step intersects with what the argument can already be on its
    /// way down — so an operator with no inverse hands back the range it was
    /// given. Identical information, and it means a test for "narrows nothing"
    /// has to compare against the box.
    fn assert_narrows_nothing(source: &str, wanted: &str, range: (f64, f64), held: &[(&str, f64)]) {
        let ratio = narrowing_ratio(source, wanted, range, held);
        assert!(
            ratio >= 1.0,
            "{source}: narrowed {wanted} to {ratio} of its range, but nothing              here has an inverse to narrow it with"
        );
    }

    /// The case the whole design is for: an equality over a compound side,
    /// where the coordinate's slice is the tolerance band and nothing else.
    #[test]
    fn an_equality_narrows_to_the_band_around_its_solution() {
        let source = "x1 + x2 == 3 +/- 0.1";
        let held = &[("x2", 1.0)][..];
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), held);

        let interval = narrowed(source, "x1", (-10.0, 10.0), held);
        assert!(interval.contains(1.9) && interval.contains(2.1));
        assert!(!interval.contains(1.8) && !interval.contains(2.2));
    }

    /// **What driving could not do.** An inequality has no tolerance and no
    /// isolated variable, so `classify` reads nothing from it — and it still
    /// says exactly where `x1` may go.
    #[test]
    fn an_inequality_narrows_to_a_half_line() {
        assert_no_feasible_value_is_excluded("x1 + x2 < 3", "x1", (-10.0, 10.0), &[("x2", 1.0)]);
        let interval = narrowed("x1 + x2 < 3", "x1", (-10.0, 10.0), &[("x2", 1.0)]);
        assert!(interval.contains(1.99) && !interval.contains(2.01));
    }

    #[test]
    fn a_lower_bound_narrows_from_below() {
        assert_no_feasible_value_is_excluded("x1 + x2 > 3", "x1", (-10.0, 10.0), &[("x2", 1.0)]);
        let interval = narrowed("x1 + x2 > 3", "x1", (-10.0, 10.0), &[("x2", 1.0)]);
        assert!(interval.contains(2.01) && !interval.contains(1.99));
    }

    /// One of the two arms where the operands do not commute: `a - u` inverts
    /// to `a - T` and not `T - a`. A swap here is silently wrong rather than a
    /// compile error, which is the same trap `classify::isolate` documents.
    #[test]
    fn subtraction_on_the_right_keeps_its_operand_order() {
        assert_no_feasible_value_is_excluded("3 - x1 == 1 +/- 0.1", "x1", (-10.0, 10.0), &[]);
        let interval = narrowed("3 - x1 == 1 +/- 0.1", "x1", (-10.0, 10.0), &[]);
        assert!(interval.contains(2.0), "{interval:?} should sit around 2");
    }

    /// The other one.
    #[test]
    fn division_on_the_right_keeps_its_operand_order() {
        assert_no_feasible_value_is_excluded("12 / x1 == 4 +/- 0.1", "x1", (0.5, 10.0), &[]);
        let interval = narrowed("12 / x1 == 4 +/- 0.1", "x1", (0.5, 10.0), &[]);
        assert!(interval.contains(3.0), "{interval:?} should sit around 3");
    }

    /// `u + b == c` narrows `u` to `c - b`, the commuting arm.
    #[test]
    fn addition_narrows_through_subtraction() {
        let source = "x1 + x2 == 3 +/- 0.1";
        let held = &[("x2", 4.0)][..];
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), held);
        assert!(narrowed(source, "x1", (-10.0, 10.0), held).contains(3.0 - 4.0));
    }

    /// `u - b == c` narrows `u` to `c + b`.
    #[test]
    fn subtraction_on_the_left_narrows_through_addition() {
        let source = "x1 - x2 == 1 +/- 0.1";
        let held = &[("x2", 4.0)][..];
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), held);
        assert!(narrowed(source, "x1", (-10.0, 10.0), held).contains(1.0 + 4.0));
    }

    /// `u / b == c` narrows `u` to `c * b`, the commuting division arm.
    #[test]
    fn division_on_the_left_narrows_through_multiplication() {
        let source = "x1 / x2 == 4 +/- 0.1";
        let held = &[("x2", 3.0)][..];
        assert_no_feasible_value_is_excluded(source, "x1", (0.5, 40.0), held);
        assert!(narrowed(source, "x1", (0.5, 40.0), held).contains(4.0 * 3.0));
    }

    /// Unary minus, which is its own inverse and is a separate arm from the
    /// binary subtraction below it.
    #[test]
    fn negation_narrows_through_itself() {
        assert_no_feasible_value_is_excluded("-x1 == 5 +/- 0.1", "x1", (-10.0, 10.0), &[]);
        assert!(narrowed("-x1 == 5 +/- 0.1", "x1", (-10.0, 10.0), &[]).contains(-5.0));
    }

    /// `0 - u` is a *subtraction*, not a negation, so it reaches the binary arm
    /// and [`negation_narrows_through_itself`] covers the unary one. Two tests
    /// because an earlier version of this pair had only one and believed it
    /// covered both.
    #[test]
    fn a_subtraction_from_zero_is_not_a_negation() {
        assert_no_feasible_value_is_excluded("0 - x1 == 5 +/- 0.1", "x1", (-10.0, 10.0), &[]);
        assert!(narrowed("0 - x1 == 5 +/- 0.1", "x1", (-10.0, 10.0), &[]).contains(0.0 - 5.0));
    }

    /// Narrowing past a term babel and Rust have to agree on, which puts the
    /// evaluator under test here and not only the rearrangement. The band for
    /// `y` is `3 - sin(1)`, computed by Rust rather than written out.
    #[test]
    fn narrowing_past_a_transcendental_agrees_with_rust() {
        // The tolerance is wide enough for the sweep's grid to land inside it.
        // At `+/- 0.001` over a range of twenty the scan steps clean over the
        // band, finds nothing feasible, and asserts nothing — which is what its
        // own guard reported.
        let source = "sin(x) + y == 3 +/- 0.05";
        let held = &[("x", 1.0)][..];
        assert_no_feasible_value_is_excluded(source, "y", (-10.0, 10.0), held);

        let interval = narrowed(source, "y", (-10.0, 10.0), held);
        let wanted = 3.0 - 1.0_f64.sin();
        assert!(
            interval.contains(wanted),
            "{interval:?} should hold {wanted}"
        );
        assert!(interval.width() < 0.11, "{interval:?} should be the band");
    }

    #[test]
    fn multiplication_narrows_through_division() {
        let source = "x1 * x2 == 12 +/- 0.1";
        let held = &[("x2", 4.0)][..];
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), held);
        assert!(narrowed(source, "x1", (-10.0, 10.0), held).contains(3.0));
    }

    /// The cross. Standing on the arm where `x2` is zero, `x1` really is
    /// unconstrained — this answers `ENTIRE` because that is the truth and not
    /// because it failed. Reaching the other arm is a connectivity problem,
    /// which is why `both_arms_of_a_product_receive_points` stays red.
    #[test]
    fn a_product_against_zero_narrows_nothing_from_the_axis() {
        let source = "x1 * x2 == 0 +/- 0.001";
        let held = &[("x2", 0.0)][..];
        assert_narrows_nothing(source, "x1", (-10.0, 10.0), held);
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), held);
    }

    /// Narrowing through a function's inverse, which is where this reaches past
    /// what rearranging a comparison can do.
    #[test]
    fn a_logarithm_narrows_through_its_inverse() {
        assert_no_feasible_value_is_excluded("ln(x1) < 2", "x1", (0.1, 20.0), &[]);
        let interval = narrowed("ln(x1) < 2", "x1", (0.1, 20.0), &[]);
        assert!(
            interval.hi() > 7.3 && interval.hi() < 7.4,
            "{interval:?} should stop just above e squared"
        );
    }

    /// An even function's inverse has two branches, and the argument's own
    /// range is what picks one. Without that the hull spans zero and the
    /// narrowing is worth far less.
    #[test]
    fn an_even_function_uses_the_argument_range_to_pick_its_branch() {
        let source = "sqr(x1) == 9 +/- 0.1";
        assert_no_feasible_value_is_excluded(source, "x1", (0.0, 10.0), &[]);

        let positive = narrowed(source, "x1", (0.0, 10.0), &[]);
        assert!(positive.contains(3.0) && !positive.contains(-3.0));

        let negative = narrowed(source, "x1", (-10.0, 0.0), &[]);
        assert!(negative.contains(-3.0) && !negative.contains(3.0));

        // Both of the above hold for the *hull* of the two branches clipped to
        // the argument's range — `[-3.05, 0]` contains -3 and excludes 3 — so
        // they cannot tell branch selection from its absence. The width can:
        // picking a branch gives a sliver either side of 3, and taking the hull
        // gives a third of the box.
        for range in [(0.0, 10.0), (-10.0, 0.0)] {
            let ratio = narrowing_ratio(source, "x1", range, &[]);
            assert!(
                ratio < 0.01,
                "{source} over {range:?}: narrowed to {ratio} of the box, which                  is the hull of both branches rather than the live one"
            );
        }
    }

    /// A sum narrows each term against the target less the others, so an
    /// unrolled aggregate is not opaque.
    #[test]
    fn a_sum_narrows_each_of_its_terms() {
        let source = "sum(1, 3, i -> x1 * i) == 12 +/- 0.1";
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), &[]);
        assert!(narrowed(source, "x1", (-10.0, 10.0), &[]).contains(2.0));
    }

    /// A `let` binding, where the target has to travel back through the
    /// assignment rather than only down the result expression.
    #[test]
    fn a_target_reaches_back_through_a_let_binding() {
        let source = "var doubled = x1 * 2;\n doubled + x2 == 8 +/- 0.1";
        let held = &[("x2", 2.0)][..];
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), held);
        assert!(narrowed(source, "x1", (-10.0, 10.0), held).contains(3.0));
        assert!(
            narrowing_ratio(source, "x1", (-10.0, 10.0), held) < 0.05,
            "the binding blocked the narrowing"
        );
    }

    /// An implicit equality is narrowed from each occurrence separately, and
    /// their intersection is a necessary condition rather than a solution.
    /// Sound, weaker than a solve, and it must not exclude the fixed point.
    #[test]
    fn a_variable_on_both_sides_narrows_soundly_without_solving() {
        let source = "x1 == x1 / 2 + 1 +/- 0.01";
        assert_no_feasible_value_is_excluded(source, "x1", (-10.0, 10.0), &[]);
        assert!(narrowed(source, "x1", (-10.0, 10.0), &[]).contains(2.0));
    }

    /// The operators with no inverse in the table narrow nothing, which is the
    /// safe answer and leaves the caller with the declared box.
    #[test]
    fn an_operator_without_an_inverse_narrows_nothing() {
        for source in [
            "x1 ^ 2.5 == 4 +/- 0.1",
            "max(x1, x2) == 3 +/- 0.1",
            "x1 % 3 == 1 +/- 0.1",
            "sin(x1) == 0.5 +/- 0.01",
        ] {
            assert_narrows_nothing(source, "x1", (-10.0, 10.0), &[("x2", 1.0)]);
        }
    }

    /// The bound must not depend on how the power was spelled: `h * h` is
    /// `h^2` and narrows through its root, not through `Mul`'s inverse, which
    /// divides by `h`'s whole range and concludes nothing.
    #[test]
    fn a_product_of_a_term_with_itself_narrows_as_its_power() {
        let spelled_as_product = narrowed("h * h < 9", "h", (0.0, 10.0), &[]);
        let spelled_as_power = narrowed("h^2 < 9", "h", (0.0, 10.0), &[]);
        assert!(
            spelled_as_power.hi() >= 3.0 && spelled_as_power.hi() < 3.0 + 1e-9,
            "{spelled_as_power:?}"
        );
        assert_eq!(spelled_as_product, spelled_as_power);
    }

    /// A whole power narrows like `sqr` and `cube`: the even root picks its
    /// branch from the base's own range, the odd one is monotone, and a
    /// negative exponent goes through the reciprocal first. `x^2` is how every
    /// optimizer formulation spells it, and until this was inverted a repair
    /// against `x^2 + y^2 < 1` had nothing to clamp to.
    #[test]
    fn a_whole_power_narrows_through_its_root() {
        let held = &[("x2", 4.0)][..];
        for (source, range) in [
            ("x1 ^ 2 == 4 +/- 0.1", (-10.0, 10.0)),
            ("x1 ^ 3 == 8 +/- 0.1", (-10.0, 10.0)),
            ("x1 ^ 5 == -32 +/- 0.1", (-10.0, 10.0)),
            ("x1 ^ 4 + x2 == 20 +/- 0.1", (-10.0, 10.0)),
            ("x1 ^ -2 == 0.25 +/- 0.01", (0.1, 10.0)),
            ("x1 ^ -3 < 1", (-10.0, 10.0)),
            ("x1 ^ 64 < 2", (-10.0, 10.0)),
            ("(x1 + 1) ^ 2 < 4", (-10.0, 10.0)),
        ] {
            assert_no_feasible_value_is_excluded(source, "x1", range, held);
        }

        let source = "x1 ^ 2 == 9 +/- 0.1";
        let positive = narrowed(source, "x1", (0.0, 10.0), &[]);
        assert!(positive.contains(3.0) && !positive.contains(-3.0));
        let negative = narrowed(source, "x1", (-10.0, 0.0), &[]);
        assert!(negative.contains(-3.0) && !negative.contains(3.0));

        // Over a box straddling zero both branches are live and the hull is
        // `[-3.02, 3.02]`: a fiftieth of the box, where a fold of two copies
        // of `x1` narrowed nothing at all.
        let hull = narrowing_ratio(source, "x1", (-100.0, 100.0), &[]);
        assert!(hull < 0.05, "{source}: narrowed to {hull} of the box");
        let cubed = narrowing_ratio("x1 ^ 3 == 8 +/- 0.1", "x1", (-100.0, 100.0), &[]);
        assert!(
            cubed < 0.01,
            "an odd power is monotone and should narrow to a sliver, got {cubed}"
        );
    }

    /// Narrowing is only worth having if it narrows. A regression to `ENTIRE`
    /// would still be *sound*, and would silently undo the entire point — which
    /// is the failure this module is most exposed to.
    #[test]
    fn the_narrowing_is_worth_computing() {
        for (source, held) in [
            ("x1 + x2 == 3 +/- 0.1", &[("x2", 1.0)][..]),
            ("x1 - x2 == 1 +/- 0.1", &[("x2", 4.0)][..]),
            ("x1 * x2 == 12 +/- 0.1", &[("x2", 4.0)][..]),
            ("x1 / x2 == 4 +/- 0.1", &[("x2", 3.0)][..]),
            ("3 - x1 == 1 +/- 0.1", &[][..]),
        ] {
            let ratio = narrowing_ratio(source, "x1", (-100.0, 100.0), held);
            assert!(
                ratio < 0.01,
                "{source}: narrowed to {ratio} of the box, which is barely a narrowing"
            );
        }
    }

    /// This crate now states each function's inverse twice — here as an
    /// interval preimage, and in `rewrite::monotone` as a rearrangement of a
    /// comparison against a constant. Facts written down twice drift, so this
    /// is what holds the two together: `f(x) < c` rearranges to
    /// `x < f_inverse(c)`, and narrowing `f(x)` into `(-inf, c]` has to reach
    /// the same bound.
    #[test]
    fn the_two_inverse_tables_agree() {
        for (source, bound) in [
            ("ln(x1) < 2", std::f64::consts::E * std::f64::consts::E),
            ("sqrt(x1) < 3", 9.0),
            ("atan(x1) < 1", 1.0_f64.tan()),
            ("sinh(x1) < 2", 2.0_f64.asinh()),
            ("tanh(x1) < 0.5", 0.5_f64.atanh()),
            ("cbrt(x1) < 2", 8.0),
        ] {
            let interval = narrowed(source, "x1", (-100.0, 100.0), &[]);
            assert!(
                (interval.hi() - bound).abs() < 1e-9,
                "{source}: narrowing stops at {} where the inversion table says {bound}",
                interval.hi()
            );
        }
    }
}

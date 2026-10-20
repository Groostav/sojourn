//! The values one coordinate may take: an interval, or a list of levels.
//!
//! Derived once per variable from its declared [`Domain`] by
//! `ConstraintSystem::new`, which is where an ill-formed domain panics, and
//! the one implementation of level arithmetic in the crate: membership, the
//! nearest level, rounding an interval inward onto levels, and splitting one
//! between two adjacent levels. Everything that judges or draws a coordinate
//! goes through here rather than through the declaration, which the engine
//! never re-reads.
//!
//! # Why "support"
//!
//! In probability, the *support* of a random variable is the set of values it
//! can take — the set its distribution puts weight on. A uniform variable on
//! `[0, 1]` has the interval as its support; a fair die has `{1, …, 6}`. That
//! is exactly what this is for each coordinate of a point, and sojourn's
//! points are random draws, so the statistician's word is the plain one here.
//!
//! It is not the [`Domain`], though it is made from one. A domain is what the
//! caller wrote — a declaration, kept as given and read back for names and
//! messages. A support is that declaration checked, and precomputed into the
//! form arithmetic on it needs: a lattice's decimal integers are worked out
//! once here, because doing it per judgement means formatting floats. And the
//! two can differ on purpose: the relaxation behind
//! `Infeasibility::BetweenLevels` is the same system with every discrete
//! support widened to its interval, and repair's search over levels pins a
//! discrete support to one value. The declaration says what the caller asked
//! for; the support says what this search is letting a coordinate take.
//!
//! # A lattice is decimal
//!
//! `Stepped { lower: 0.0, step: 0.1, upper: 1.0 }` computed as `lower + k·step`
//! puts level 3 at `0.30000000000000004`, and `t <= 0.3` then fails on a value
//! the caller plainly declared. So a lattice is read as the decimals the caller
//! wrote — the shortest round-trip form of each bound and the step, which is
//! the literal whenever there was one — brought to a common power of ten and
//! held as integers `first`, `step` and `count`. Level `k` is
//! `(first + k·step) · 10^e`: an integer below 2^53 and a power of ten no
//! larger than 10^22 are both exact in `f64`, so that is one correctly rounded
//! operation, and its result is bit-equal to parsing the decimal (Clinger's
//! fast path). A lattice that cannot be held that way — a step of `1/3`, a
//! lower bound of `0.1 + 0.2`, sixteen significant digits on a bound — is
//! refused at construction with a pointer to [`Domain::Levels`], which takes
//! the values as given.
//!
//! # Rounding inward is sound
//!
//! An interval from [`interval`](super::interval) is a superset of what it
//! models. Rounding its ends inward onto levels removes only values the
//! coordinate could never take, so the result is still a superset of the
//! admissible ones — and it may be empty where the interval was not, which is
//! the whole point: `n` in `[2.5, 2.9]` on the integers is a proof.

use std::ops::RangeInclusive;

use crate::cvg::interval::Interval;
use crate::{Domain, InputVariable};

/// The support of one variable: the values its coordinate may take, as the
/// engine computes with them. See the module header for the word and for
/// how it differs from the [`Domain`] it is made from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VariableSupport {
    /// Every value from `lower` to `upper`, inclusive.
    Continuous { lower: f64, upper: f64 },
    /// Exactly the levels, and nothing between them.
    Discrete(Levels),
}

/// The levels of a discrete coordinate, ascending, indexed from zero.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Levels {
    /// `(first + k·step) · scale` for `k` in `0..count`. Every
    /// `first + k·step` is below 2^53 in magnitude, so its conversion to `f64`
    /// is exact; see the module header.
    Lattice {
        first: i64,
        step: i64,
        count: usize,
        scale: Scale,
    },
    /// The values as given: finite, strictly ascending, at least one.
    Listed(Vec<f64>),
}

/// The power of ten a lattice's integers are scaled by, written the way that
/// keeps the operation exact: dividing by `10^e` rather than multiplying by
/// `10^-e`, which is not representable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Scale {
    Divide(f64),
    Multiply(f64),
}

/// The largest power of ten that is exact in `f64`, and the largest integer
/// magnitude that is.
const EXACT_POWER: i32 = 22;
const EXACT_INTEGER: i128 = 1 << 53;

impl VariableSupport {
    /// The support `variable` declares.
    ///
    /// # Panics
    /// If the declaration is ill-formed: bounds that are not finite or are
    /// the wrong way round, a step that is not positive and finite or does not
    /// land on the upper bound, a lattice too fine to hold exactly, or levels
    /// that are empty, not finite or not strictly ascending. Each message names
    /// the variable; these are the caller's to check, as
    /// `ConstraintSystem::new` documents.
    pub(crate) fn of(variable: &InputVariable) -> Self {
        let name = &variable.name;
        match &variable.domain {
            Domain::Continuous { lower, upper } => {
                assert_bounds(name, *lower, *upper);
                Self::Continuous {
                    lower: *lower,
                    upper: *upper,
                }
            }
            Domain::Stepped { lower, step, upper } => {
                assert_bounds(name, *lower, *upper);
                Self::Discrete(lattice(name, *lower, *step, *upper))
            }
            Domain::Levels(levels) => {
                assert!(!levels.is_empty(), "variable {name:?} has no levels");
                for level in levels {
                    assert!(
                        level.is_finite(),
                        "variable {name:?} has a level {level}; levels must be finite"
                    );
                }
                for pair in levels.windows(2) {
                    let [before, after] = [pair[0], pair[1]];
                    assert!(
                        before != after,
                        "variable {name:?} lists the level {before} twice"
                    );
                    assert!(
                        before < after,
                        "variable {name:?} lists the level {after} after {before}; levels must be strictly ascending"
                    );
                }
                Self::Discrete(Levels::Listed(levels.clone()))
            }
        }
    }

    pub(crate) fn lower(&self) -> f64 {
        match self {
            Self::Continuous { lower, .. } => *lower,
            Self::Discrete(levels) => levels.level(0),
        }
    }

    pub(crate) fn upper(&self) -> f64 {
        match self {
            Self::Continuous { upper, .. } => *upper,
            Self::Discrete(levels) => levels.level(levels.count() - 1),
        }
    }

    /// Whether `value` is one this coordinate may take: inside the bounds, and
    /// for a discrete coordinate exactly on a level.
    pub(crate) fn contains(&self, value: f64) -> bool {
        match self {
            Self::Continuous { lower, upper } => (*lower..=*upper).contains(&value),
            Self::Discrete(levels) => levels.index_of(value).is_some(),
        }
    }

    /// `interval` with its ends moved inward onto levels; unchanged for a
    /// continuous coordinate. Sound for the reason in the module header.
    pub(crate) fn round_inward(&self, interval: Interval) -> Interval {
        match self {
            Self::Continuous { .. } => interval,
            Self::Discrete(levels) => levels.round_inward(interval),
        }
    }
}

impl Levels {
    pub(crate) fn count(&self) -> usize {
        match self {
            Self::Lattice { count, .. } => *count,
            Self::Listed(levels) => levels.len(),
        }
    }

    /// Level `k`, counting from the lowest.
    ///
    /// # Panics
    /// If `k` is not below [`count`](Self::count).
    pub(crate) fn level(&self, k: usize) -> f64 {
        assert!(
            k < self.count(),
            "level {k} of {} does not exist",
            self.count()
        );
        match self {
            Self::Lattice {
                first, step, scale, ..
            } => {
                let k = i64::try_from(k).expect("a lattice is shorter than 2^53 levels");
                // Below 2^53 in magnitude by construction, so exact.
                let integer = (first + k * step) as f64;
                match scale {
                    Scale::Divide(power) => integer / power,
                    Scale::Multiply(power) => integer * power,
                }
            }
            Self::Listed(levels) => levels[k],
        }
    }

    /// The index of the level `value` is exactly, if it is one.
    pub(crate) fn index_of(&self, value: f64) -> Option<usize> {
        let around = self.estimate(value)?;
        let candidates = around.saturating_sub(1)..=(around + 1).min(self.count() - 1);
        // `==` rather than bits: -0.0 and 0.0 are the same level.
        candidates.into_iter().find(|&k| self.level(k) == value)
    }

    /// The index of the level nearest `value`, the lower of two that tie.
    pub(crate) fn nearest(&self, value: f64) -> usize {
        let around = self.estimate(value).unwrap_or(0);
        let candidates = around.saturating_sub(1)..=(around + 1).min(self.count() - 1);
        candidates
            .into_iter()
            .min_by(|&a, &b| {
                (self.level(a) - value)
                    .abs()
                    .total_cmp(&(self.level(b) - value).abs())
            })
            .expect("a support has at least one level")
    }

    /// The indices of the levels inside `interval`, or `None` if it holds none.
    pub(crate) fn within(&self, interval: Interval) -> Option<RangeInclusive<usize>> {
        if interval.is_empty() {
            return None;
        }
        let first = self.at_or_above(interval.lo())?;
        let last = self.at_or_below(interval.hi())?;
        (first <= last).then_some(first..=last)
    }

    /// The smallest interval holding every level inside `interval` — empty if
    /// there is none.
    pub(crate) fn round_inward(&self, interval: Interval) -> Interval {
        match self.within(interval) {
            Some(range) => Interval::new(self.level(*range.start()), self.level(*range.end())),
            None => Interval::EMPTY,
        }
    }

    /// `interval`'s levels in two halves that share none, each ending on a
    /// level: what bisection splits a discrete coordinate into. `None` when
    /// it holds fewer than two levels and there is nothing to split.
    pub(crate) fn split(&self, interval: Interval) -> Option<(Interval, Interval)> {
        let range = self.within(interval)?;
        let (first, last) = (*range.start(), *range.end());
        if first == last {
            return None;
        }
        let middle = first + (last - first) / 2;
        Some((
            Interval::new(self.level(first), self.level(middle)),
            Interval::new(self.level(middle + 1), self.level(last)),
        ))
    }

    /// The first index whose level is at least `bound`.
    fn at_or_above(&self, bound: f64) -> Option<usize> {
        let mut k = self.estimate(bound).unwrap_or(0);
        // The estimate is within one of the answer; these settle it exactly.
        while k > 0 && self.level(k - 1) >= bound {
            k -= 1;
        }
        while k < self.count() && self.level(k) < bound {
            k += 1;
        }
        (k < self.count()).then_some(k)
    }

    /// The last index whose level is at most `bound`.
    fn at_or_below(&self, bound: f64) -> Option<usize> {
        let mut k = self.estimate(bound).unwrap_or(0);
        // Settled to the first level above `bound`, then one back.
        while k > 0 && self.level(k - 1) > bound {
            k -= 1;
        }
        while k < self.count() && self.level(k) <= bound {
            k += 1;
        }
        k.checked_sub(1)
    }

    /// An index within one of the level nearest `value`, clamped into the
    /// support, which callers settle by comparing levels: arithmetic on a
    /// lattice, a binary search on a list. `None` for a value that is not a
    /// number.
    fn estimate(&self, value: f64) -> Option<usize> {
        if value.is_nan() {
            return None;
        }
        let last = self.count() - 1;
        match self {
            Self::Lattice { .. } => {
                let lowest = self.level(0);
                let spacing = if last > 0 {
                    self.level(1) - lowest
                } else {
                    1.0
                };
                // Clamped as a float, so an infinite bound lands on an end
                // rather than saturating a cast.
                let index = ((value - lowest) / spacing).round().clamp(0.0, last as f64);
                Some(index as usize)
            }
            Self::Listed(levels) => Some(levels.partition_point(|level| *level < value).min(last)),
        }
    }
}

/// The bounds every domain shares a check for.
fn assert_bounds(name: &str, lower: f64, upper: f64) {
    assert!(
        lower.is_finite() && upper.is_finite(),
        "variable {name:?} ranges over [{lower}, {upper}]; bounds must be finite"
    );
    assert!(
        lower <= upper,
        "variable {name:?} has lower bound {lower} above its upper bound {upper}"
    );
}

/// The lattice `lower, lower + step, …, upper`, held exactly as described in
/// the module header.
fn lattice(name: &str, lower: f64, step: f64, upper: f64) -> Levels {
    assert!(
        step.is_finite() && step > 0.0,
        "variable {name:?} steps by {step}; a step must be positive and finite"
    );
    let parts = [decimal(lower), decimal(step), decimal(upper)];
    let exponent = parts
        .iter()
        .filter(|(digits, _)| *digits != 0)
        .map(|(_, exponent)| *exponent)
        .min()
        .expect("the step is positive, so not every part is zero");
    let too_fine = || {
        format!(
            "variable {name:?} steps by {step} from {lower} to {upper}, which is too fine to \
             index exactly in f64; declare its values with Domain::Levels instead"
        )
    };
    let scaled = |(digits, own): (i64, i32)| -> Option<i128> {
        let power = 10_i128.checked_pow(u32::try_from(own - exponent).ok()?)?;
        let integer = i128::from(digits).checked_mul(power)?;
        (integer.abs() < EXACT_INTEGER).then_some(integer)
    };
    let (Some(first), Some(spacing), Some(last)) =
        (scaled(parts[0]), scaled(parts[1]), scaled(parts[2]))
    else {
        panic!("{}", too_fine());
    };
    assert!(exponent.abs() <= EXACT_POWER, "{}", too_fine());
    assert!(
        (last - first) % spacing == 0,
        "variable {name:?} steps by {step} from {lower}, which never lands on its upper bound \
         {upper}; declare its values with Domain::Levels if they are not evenly spaced"
    );
    let power = 10_f64.powi(exponent.abs());
    Levels::Lattice {
        first: i64::try_from(first).expect("below 2^53"),
        step: i64::try_from(spacing).expect("below 2^53"),
        count: usize::try_from((last - first) / spacing + 1).expect("below 2^53"),
        scale: if exponent < 0 {
            Scale::Divide(power)
        } else {
            Scale::Multiply(power)
        },
    }
}

/// `value` as `digits · 10^exponent`, from its shortest round-trip decimal:
/// what the caller wrote, whenever what they wrote was a literal. Zero, and
/// negative zero with it, is `(0, 0)`.
fn decimal(value: f64) -> (i64, i32) {
    let written = format!("{value:e}");
    let (mantissa, exponent) = written
        .split_once('e')
        .expect("`{:e}` always writes an exponent");
    let exponent: i32 = exponent.parse().expect("`{:e}` writes a decimal exponent");
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits: i64 = format!("{whole}{fraction}")
        .parse()
        .expect("seventeen significant digits fit an i64");
    let fraction = i32::try_from(fraction.len()).expect("a mantissa is short");
    (digits, exponent - fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discrete(domain: Domain) -> Levels {
        match VariableSupport::of(&InputVariable::new("x", domain)) {
            VariableSupport::Discrete(levels) => levels,
            VariableSupport::Continuous { .. } => panic!("the fixture declares a discrete domain"),
        }
    }

    fn stepped(lower: f64, step: f64, upper: f64) -> Levels {
        discrete(Domain::Stepped { lower, step, upper })
    }

    fn levels(levels: &Levels) -> Vec<f64> {
        (0..levels.count()).map(|k| levels.level(k)).collect()
    }

    /// The level is the literal: `0.1 * 3` is not `0.3`, and a lattice
    /// declared in tenths must hold `0.3`, or `t <= 0.3` fails on a declared
    /// value.
    #[test]
    fn a_decimal_step_lands_on_the_literals() {
        // setup
        let tenths = stepped(0.0, 0.1, 1.0);
        let quarters = stepped(-1.5, 0.25, 1.0);
        let large = stepped(1e18, 1e18, 5e18);

        // act
        let (tenths, quarters, large) = (levels(&tenths), levels(&quarters), levels(&large));

        // assert
        let literals: Vec<f64> = [
            "0.0", "0.1", "0.2", "0.3", "0.4", "0.5", "0.6", "0.7", "0.8", "0.9", "1.0",
        ]
        .iter()
        .map(|literal| literal.parse().unwrap())
        .collect();
        assert_eq!(tenths, literals);
        assert_eq!(
            quarters,
            vec![
                -1.5, -1.25, -1.0, -0.75, -0.5, -0.25, 0.0, 0.25, 0.5, 0.75, 1.0
            ]
        );
        assert_eq!(large, vec![1e18, 2e18, 3e18, 4e18, 5e18]);
    }

    #[test]
    fn a_negative_zero_lower_bound_is_level_zero() {
        // setup
        let lattice = stepped(-0.0, 1.0, 3.0);

        // act
        let first = lattice.level(0);

        // assert
        assert_eq!(first.to_bits(), 0.0_f64.to_bits());
        assert_eq!(lattice.index_of(-0.0), Some(0));
    }

    #[test]
    fn membership_is_exact() {
        // setup
        let tenths = stepped(0.0, 0.1, 1.0);
        let listed = discrete(Domain::Levels(vec![0.25, 0.5, 1.0, 2.0]));

        // act, assert
        assert_eq!(tenths.index_of(0.3), Some(3));
        assert_eq!(tenths.index_of(0.1 * 3.0), None, "0.30000000000000004");
        assert_eq!(tenths.index_of(0.35), None);
        assert_eq!(tenths.index_of(1.1), None, "past the upper bound");
        assert_eq!(tenths.index_of(-0.1), None, "below the lower bound");
        assert_eq!(listed.index_of(1.0), Some(2));
        assert_eq!(listed.index_of(1.5), None);
    }

    #[test]
    fn the_nearest_level_takes_the_lower_of_a_tie() {
        // setup
        let integers = stepped(0.0, 1.0, 10.0);
        let listed = discrete(Domain::Levels(vec![0.25, 0.5, 1.0, 2.0]));

        // act, assert
        assert_eq!(integers.nearest(2.4), 2);
        assert_eq!(integers.nearest(2.5), 2);
        assert_eq!(integers.nearest(-7.0), 0);
        assert_eq!(integers.nearest(99.0), 10);
        assert_eq!(listed.nearest(1.4), 2);
        assert_eq!(listed.nearest(1.6), 3);
        assert_eq!(listed.nearest(0.0), 0);
    }

    #[test]
    fn rounding_inward_can_empty() {
        // setup
        let integers = stepped(0.0, 1.0, 10.0);

        // act
        let between = integers.round_inward(Interval::new(2.5, 2.9));
        let across = integers.round_inward(Interval::new(2.5, 4.1));
        let everything = integers.round_inward(Interval::ENTIRE);
        let on_levels = integers.round_inward(Interval::new(3.0, 3.0));

        // assert
        assert!(between.is_empty(), "{between:?}");
        assert_eq!(across, Interval::new(3.0, 4.0));
        assert_eq!(everything, Interval::new(0.0, 10.0));
        assert_eq!(on_levels, Interval::new(3.0, 3.0));
    }

    #[test]
    fn rounding_inward_onto_listed_levels() {
        // setup
        let listed = discrete(Domain::Levels(vec![0.25, 0.5, 1.0, 2.0]));

        // act
        let between = listed.round_inward(Interval::new(0.6, 0.9));
        let across = listed.round_inward(Interval::new(0.3, 1.5));

        // assert
        assert!(between.is_empty(), "{between:?}");
        assert_eq!(across, Interval::new(0.5, 1.0));
    }

    #[test]
    fn a_split_is_disjoint_between_adjacent_levels() {
        // setup
        let integers = stepped(0.0, 1.0, 10.0);

        // act
        let halves = integers.split(Interval::new(2.5, 7.0));
        let single = integers.split(Interval::new(2.5, 3.5));

        // assert
        assert_eq!(
            halves,
            Some((Interval::new(3.0, 5.0), Interval::new(6.0, 7.0)))
        );
        assert_eq!(single, None, "one level has nothing to split");
    }

    #[test]
    fn a_continuous_support_is_its_bounds() {
        // setup
        let support = VariableSupport::of(&InputVariable::new_continuous("x", -1.0, 2.0));

        // act, assert
        assert!(support.contains(0.123));
        assert!(!support.contains(2.5));
        assert_eq!(
            support.round_inward(Interval::new(0.1, 0.2)),
            Interval::new(0.1, 0.2)
        );
    }
}

//! How good a design is, as numbers rather than thresholds.
//!
//! The pool tests say whether a design is broken — a piece of the region
//! with no point in it, two points that coincide — and nothing about whether
//! one design is better than another, or whether a change made designs worse
//! while every threshold still passed. These are the measurements that can:
//! pure functions of a design and a [`Reference`], the distribution the
//! design is meant to cover.
//!
//! | measure | what it catches |
//! |---|---|
//! | [`strata`] | a frozen or bunched coordinate; `1.0` is Latin on that axis |
//! | [`level_shares`] | a discrete level over- or under-represented |
//! | [`cl2`] | unevenness overall, weighted toward one- and two-axis projections |
//! | [`min_distance`] | clumping, and points that coincide |
//! | [`covering_radius`] | a part of the region far from every design point |
//! | [`allocation`] | a whole piece of the region skipped, or starved |
//!
//! Every measure reads box-normalised coordinates, `(x - lower) / width`
//! per variable, the scale [`FeasibleRegion::repair`](crate::FeasibleRegion::repair)
//! lands by and a design is spread by; a zero-width variable sits at `0.5`
//! and contributes nothing. A design is one column per point and one row per
//! variable, the shape [`FeasibleRegion::sample`](crate::FeasibleRegion::sample)
//! returns.
//!
//! **The reference must not come from the thing being measured.** On a
//! constrained region the only reference sojourn has is its own pool, and a
//! biased walker grades itself well against what it walked. So the
//! benchmarks that judge sojourn's designs draw their reference from regions
//! whose distribution is known in closed form, by samplers written without
//! sojourn (`tests/sampling_quality.rs`), and [`Reference::uniform`] is the
//! closed form itself for a box. A report against a pool is for users, once a
//! pool is a public thing; the plan is `docs/sampling-quality.md`.
//!
//! The discrepancy is Hickernell's centred L2 (CL2, 1998). Its kernel is a
//! product over axes, so the squared discrepancy decomposes into a weighted
//! sum over every projection of the design onto a subset of the axes, the
//! low-order ones weighted most. That is why it stays meaningful at a hundred
//! points in twenty dimensions, where the joint space is empty and no grid
//! over it is populated. Against a sample it is the same kernel read as a
//! maximum mean discrepancy.

use faer::MatRef;

use crate::ConstraintSystem;
use crate::cvg::support::VariableSupport;

/// The distribution a design is meant to cover, on the box of the system it
/// was built for.
///
/// A value, built once and read by every measure: a sample reference sorts
/// its marginals and sums its kernel over every pair at construction, which
/// is `O(m² · d)` and the reason not to rebuild it per design.
#[derive(Debug, Clone)]
pub struct Reference {
    lower: Vec<f64>,
    width: Vec<f64>,
    /// Whether each axis is declared discrete: what [`level_shares`] reads,
    /// and what [`strata`] leaves out.
    discrete: Vec<bool>,
    distribution: Distribution,
}

#[derive(Debug, Clone)]
enum Distribution {
    /// Uniform over the declared box: a product of one measure per axis, so
    /// every kernel expectation is a product of one-dimensional ones.
    Uniform(Vec<Axis>),
    /// The empirical measure of these points.
    Sample {
        /// Normalised, one `Vec` per point.
        points: Vec<Vec<f64>>,
        /// Each continuous axis's normalised values, ascending: the
        /// empirical marginal CDF. `None` on a discrete or zero-width axis.
        marginals: Vec<Option<Vec<f64>>>,
        /// The kernel's mean over every ordered pair of points, the diagonal
        /// included: `E k(Y, Y')` under the empirical measure, exactly.
        mean_kernel: f64,
        /// The kernel's mean at coincident points, `E k(Y, Y)`.
        mean_diagonal: f64,
    },
}

/// One axis of a uniform reference, in normalised coordinates.
#[derive(Debug, Clone)]
enum Axis {
    /// Uniform on `[0, 1]`.
    Continuous,
    /// Uniform over these positions, ascending, with the running sums the
    /// kernel's expectations are read from in `O(log L)`. A zero-width
    /// continuous axis is the one position `0.5`.
    Atoms {
        positions: Vec<f64>,
        prefix: Vec<f64>,
    },
}

impl Axis {
    fn atoms(positions: Vec<f64>) -> Self {
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "the positions of a discrete axis ascend strictly"
        );
        let prefix = std::iter::once(0.0)
            .chain(positions.iter().scan(0.0, |sum, position| {
                *sum += position;
                Some(*sum)
            }))
            .collect();
        Self::Atoms { positions, prefix }
    }

    /// `E |z - Z|` for `Z` drawn from this axis.
    fn mean_gap(&self, z: f64) -> f64 {
        match self {
            Self::Continuous => (z * z + (1.0 - z) * (1.0 - z)) / 2.0,
            Self::Atoms { positions, prefix } => {
                let count = positions.len() as f64;
                let below = positions.partition_point(|position| *position <= z);
                let total = prefix[positions.len()];
                let under = below as f64 * z - prefix[below];
                let over = (total - prefix[below]) - (positions.len() - below) as f64 * z;
                (under + over) / count
            }
        }
    }

    /// `E |Z - ½|`.
    fn mean_offset(&self) -> f64 {
        match self {
            Self::Continuous => 0.25,
            Self::Atoms { positions, .. } => {
                positions.iter().map(|z| (z - 0.5).abs()).sum::<f64>() / positions.len() as f64
            }
        }
    }

    /// `E |Z - Z'|` for two independent draws.
    fn mean_pair_gap(&self) -> f64 {
        match self {
            Self::Continuous => 1.0 / 3.0,
            Self::Atoms { positions, prefix } => {
                // Each position's gap to every one below it, summed once,
                // counts each unordered pair once; doubled for the ordered.
                let count = positions.len() as f64;
                let below: f64 = positions
                    .iter()
                    .enumerate()
                    .map(|(index, position)| index as f64 * position - prefix[index])
                    .sum();
                2.0 * below / (count * count)
            }
        }
    }

    /// `E k(z, Z)` for the one-axis kernel
    /// `k(a, b) = 1 + |a - ½|/2 + |b - ½|/2 - |a - b|/2`.
    fn mean_kernel_at(&self, z: f64) -> f64 {
        1.0 + (z - 0.5).abs() / 2.0 + self.mean_offset() / 2.0 - self.mean_gap(z) / 2.0
    }

    /// `E k(Z, Z')`: `13/12` on the unit interval.
    fn mean_kernel(&self) -> f64 {
        1.0 + self.mean_offset() - self.mean_pair_gap() / 2.0
    }

    /// `E k(Z, Z)`: `5/4` on the unit interval.
    fn mean_diagonal(&self) -> f64 {
        1.0 + self.mean_offset()
    }
}

/// Lattices beyond this many levels are refused by [`Reference::uniform`]:
/// the axis holds every position, and a lattice that large is a continuous
/// variable declared the expensive way.
const LEVEL_LIMIT: usize = 1 << 24;

impl Reference {
    /// Uniform over the declared box: each continuous variable uniform on its
    /// interval, each discrete one uniform over its levels, independently.
    /// The closed form, and the right reference for a system of bounds alone.
    ///
    /// # Panics
    /// If a discrete variable has more than `2^24` levels.
    #[must_use]
    pub fn uniform(system: &ConstraintSystem) -> Self {
        let (lower, width, discrete) = scales(system);
        let axes = system
            .supports
            .iter()
            .zip(&lower)
            .zip(&width)
            .map(|((support, lower), width)| match support {
                VariableSupport::Continuous { .. } if *width > 0.0 => Axis::Continuous,
                VariableSupport::Continuous { .. } => Axis::atoms(vec![0.5]),
                VariableSupport::Discrete(levels) => {
                    assert!(
                        levels.count() <= LEVEL_LIMIT,
                        "a uniform reference holds every level, and {} is too many",
                        levels.count()
                    );
                    Axis::atoms(
                        (0..levels.count())
                            .map(|k| normalise(levels.level(k), *lower, *width))
                            .collect(),
                    )
                }
            })
            .collect();
        Self {
            lower,
            width,
            discrete,
            distribution: Distribution::Uniform(axes),
        }
    }

    /// The empirical distribution of `points`, one column per point: a large
    /// sample from the distribution a design should cover, drawn by something
    /// other than the design's own machinery.
    ///
    /// Costs `O(m² · d)` for `m` points, once: the kernel's mean over every
    /// pair is summed exactly, so a design is compared against this sample
    /// and not against an estimate of it. Twenty thousand points in twenty
    /// dimensions is a few seconds in release.
    ///
    /// # Panics
    /// If `points` has no columns, does not have one row per variable, or has
    /// a column outside the declared box or not finite. Feasibility is not
    /// checked: which region the sample describes is the caller's to say.
    #[must_use]
    pub fn sample(system: &ConstraintSystem, points: MatRef<'_, f64>) -> Self {
        let (lower, width, discrete) = scales(system);
        assert!(points.ncols() > 0, "a sample reference needs a point");
        let points = normalised(&lower, &width, points, "a reference");
        let marginals = (0..lower.len())
            .map(|axis| {
                (!discrete[axis] && width[axis] > 0.0).then(|| {
                    let mut values: Vec<f64> = points.iter().map(|point| point[axis]).collect();
                    values.sort_by(f64::total_cmp);
                    values
                })
            })
            .collect();
        let count = points.len() as f64;
        let mean_diagonal = points.iter().map(|point| kernel(point, point)).sum::<f64>() / count;
        // Symmetric, so each unordered pair once and doubled; the diagonal
        // is what `mean_diagonal` already summed.
        let off_diagonal: f64 = points
            .iter()
            .enumerate()
            .map(|(index, a)| points[..index].iter().map(|b| kernel(a, b)).sum::<f64>())
            .sum();
        let mean_kernel = (2.0 * off_diagonal + mean_diagonal * count) / (count * count);
        Self {
            lower,
            width,
            discrete,
            distribution: Distribution::Sample {
                points,
                marginals,
                mean_kernel,
                mean_diagonal,
            },
        }
    }

    /// `E k(x, Y)`: the kernel at `x` averaged over the reference.
    fn mean_kernel_at(&self, x: &[f64]) -> f64 {
        match &self.distribution {
            Distribution::Uniform(axes) => axes
                .iter()
                .zip(x)
                .map(|(axis, z)| axis.mean_kernel_at(*z))
                .product(),
            Distribution::Sample { points, .. } => {
                points.iter().map(|point| kernel(x, point)).sum::<f64>() / points.len() as f64
            }
        }
    }

    /// `E k(Y, Y')` and `E k(Y, Y)`.
    fn mean_kernels(&self) -> (f64, f64) {
        match &self.distribution {
            Distribution::Uniform(axes) => (
                axes.iter().map(Axis::mean_kernel).product(),
                axes.iter().map(Axis::mean_diagonal).product(),
            ),
            Distribution::Sample {
                mean_kernel,
                mean_diagonal,
                ..
            } => (*mean_kernel, *mean_diagonal),
        }
    }
}

/// Each variable's lower bound, width, and whether it is discrete.
fn scales(system: &ConstraintSystem) -> (Vec<f64>, Vec<f64>, Vec<bool>) {
    let lower = system.supports.iter().map(VariableSupport::lower).collect();
    let width = crate::cvg::widths(system);
    let discrete = system
        .supports
        .iter()
        .map(|support| support.levels().is_some())
        .collect();
    (lower, width, discrete)
}

fn normalise(value: f64, lower: f64, width: f64) -> f64 {
    if width > 0.0 {
        (value - lower) / width
    } else {
        0.5
    }
}

/// The columns of `points` in normalised coordinates, one `Vec` per column.
///
/// # Panics
/// If a column has the wrong length, a value is not finite, or one lies
/// outside the box: `what` names the matrix in the message.
fn normalised(lower: &[f64], width: &[f64], points: MatRef<'_, f64>, what: &str) -> Vec<Vec<f64>> {
    assert!(
        points.nrows() == lower.len(),
        "{what} has one row per variable, {}, not {}",
        lower.len(),
        points.nrows()
    );
    (0..points.ncols())
        .map(|column| {
            (0..points.nrows())
                .map(|row| {
                    let value = points[(row, column)];
                    assert!(
                        value.is_finite()
                            && value >= lower[row]
                            && value <= lower[row] + width[row],
                        "{what} has {value} at row {row} of column {column}, outside the box"
                    );
                    normalise(value, lower[row], width[row])
                })
                .collect()
        })
        .collect()
}

/// A design as normalised points, asserting it is one.
fn design_points(reference: &Reference, design: MatRef<'_, f64>) -> Vec<Vec<f64>> {
    assert!(design.ncols() > 0, "a design needs a point");
    normalised(&reference.lower, &reference.width, design, "the design")
}

/// The centred-L2 kernel, a product of one factor per axis.
fn kernel(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| 1.0 + (a - 0.5).abs() / 2.0 + (b - 0.5).abs() / 2.0 - (a - b).abs() / 2.0)
        .product()
}

fn distance(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b) * (a - b))
        .sum::<f64>()
        .sqrt()
}

/// A design's discrepancy from its reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Discrepancy {
    /// The squared discrepancy itself: against [`Reference::uniform`] on a
    /// continuous box, Hickernell's CL2². Its scale grows as `(13/12)^d`,
    /// which is why `ratio` is the number to compare.
    pub squared: f64,
    /// `squared` over what `n` independent draws from the reference would
    /// score on average, `(E k(Y,Y) - E k(Y,Y')) / n`. `1.0` is as good as
    /// random and lower is better, comparable across dimensions and counts.
    pub ratio: f64,
}

/// The design's centred-L2 discrepancy from `reference`.
///
/// `O(n² · d)` for the design's own pairs, plus `O(n · m · d)` against a
/// sample of `m` points.
///
/// # Panics
/// If `design` has no columns, not one row per variable, or a column outside
/// the box.
#[must_use]
pub fn cl2(reference: &Reference, design: MatRef<'_, f64>) -> Discrepancy {
    let points = design_points(reference, design);
    let count = points.len() as f64;
    let own: f64 = points
        .iter()
        .map(|a| points.iter().map(|b| kernel(a, b)).sum::<f64>())
        .sum::<f64>()
        / (count * count);
    let cross: f64 = points
        .iter()
        .map(|point| reference.mean_kernel_at(point))
        .sum::<f64>()
        / count;
    let (mean_kernel, mean_diagonal) = reference.mean_kernels();
    let squared = own - 2.0 * cross + mean_kernel;
    let random = (mean_diagonal - mean_kernel) / count;
    assert!(
        random > 0.0,
        "a reference that is a single point has nothing to be random over"
    );
    Discrepancy {
        squared,
        ratio: squared / random,
    }
}

/// Per variable, the fraction of the `n` equal strata of its marginal that
/// hold a design point: `1.0` is Latin on that axis, and `n` independent
/// draws score about `1 - (1 - 1/n)^n`, which is `0.634` at ten points and
/// tends to `1 - 1/e`.
///
/// Strata of the *reference's* marginal, not of the box: each coordinate is
/// read through the reference's marginal CDF first — the identity for a
/// uniform reference, the empirical CDF for a sample — so a skewed marginal
/// (a simplex's, a disc's) is stratified by its own quantiles. `None` for a
/// discrete or zero-width variable, where equal strata of a marginal with
/// atoms say nothing; [`level_shares`] answers there.
///
/// # Panics
/// As [`cl2`].
#[must_use]
pub fn strata(reference: &Reference, design: MatRef<'_, f64>) -> Vec<Option<f64>> {
    let points = design_points(reference, design);
    let count = points.len();
    (0..reference.lower.len())
        .map(|axis| {
            let cdf = |z: f64| -> Option<f64> {
                match &reference.distribution {
                    Distribution::Uniform(axes) => match axes[axis] {
                        Axis::Continuous => Some(z),
                        Axis::Atoms { .. } => None,
                    },
                    Distribution::Sample { marginals, .. } => {
                        marginals[axis].as_ref().map(|sorted| {
                            sorted.partition_point(|value| *value <= z) as f64 / sorted.len() as f64
                        })
                    }
                }
            };
            if reference.discrete[axis] {
                return None;
            }
            let mut occupied = vec![false; count];
            for point in &points {
                let u = cdf(point[axis])?;
                // `u == 1` is the top stratum's closed end.
                let stratum = ((u * count as f64) as usize).min(count - 1);
                occupied[stratum] = true;
            }
            Some(occupied.iter().filter(|held| **held).count() as f64 / count as f64)
        })
        .collect()
}

/// Per variable, the largest gap between the share of the design at a level
/// and the reference's share there, over every level either holds: `0.0` is
/// the reference's proportions exactly. `None` for a continuous variable.
///
/// # Panics
/// As [`cl2`].
#[must_use]
pub fn level_shares(reference: &Reference, design: MatRef<'_, f64>) -> Vec<Option<f64>> {
    let points = design_points(reference, design);
    (0..reference.lower.len())
        .map(|axis| {
            if !reference.discrete[axis] {
                return None;
            }
            // Keyed on bits: a level normalises to the same bits wherever it
            // came from, since the arithmetic is the same.
            let mut shares: std::collections::BTreeMap<u64, (f64, f64)> =
                std::collections::BTreeMap::new();
            let design_share = 1.0 / points.len() as f64;
            for point in &points {
                shares.entry(point[axis].to_bits()).or_default().0 += design_share;
            }
            match &reference.distribution {
                Distribution::Uniform(axes) => match &axes[axis] {
                    Axis::Atoms { positions, .. } => {
                        let share = 1.0 / positions.len() as f64;
                        for position in positions {
                            shares.entry(position.to_bits()).or_default().1 += share;
                        }
                    }
                    Axis::Continuous => unreachable!("a discrete axis is uniform over atoms"),
                },
                Distribution::Sample { points, .. } => {
                    let share = 1.0 / points.len() as f64;
                    for point in points {
                        shares.entry(point[axis].to_bits()).or_default().1 += share;
                    }
                }
            }
            Some(
                shares
                    .values()
                    .map(|(design, reference)| (design - reference).abs())
                    .fold(0.0, f64::max),
            )
        })
        .collect()
}

/// The smallest distance between two design points, in normalised
/// coordinates; infinite for a design of one point. Zero is a duplicate.
///
/// The reference supplies only the box.
///
/// # Panics
/// As [`cl2`].
#[must_use]
pub fn min_distance(reference: &Reference, design: MatRef<'_, f64>) -> f64 {
    let points = design_points(reference, design);
    points
        .iter()
        .enumerate()
        .flat_map(|(index, a)| points[..index].iter().map(move |b| distance(a, b)))
        .fold(f64::INFINITY, f64::min)
}

/// The farthest any reference point lies from its nearest design point, in
/// normalised coordinates: how far into the region one can stand and see no
/// design point. Lower is better. `None` against a uniform reference, which
/// has no points to stand on.
///
/// `O(n · m · d)`.
///
/// # Panics
/// As [`cl2`].
#[must_use]
pub fn covering_radius(reference: &Reference, design: MatRef<'_, f64>) -> Option<f64> {
    let design = design_points(reference, design);
    match &reference.distribution {
        Distribution::Uniform(_) => None,
        Distribution::Sample { points, .. } => Some(
            points
                .iter()
                .map(|point| {
                    design
                        .iter()
                        .map(|chosen| distance(point, chosen))
                        .fold(f64::INFINITY, f64::min)
                })
                .fold(0.0, f64::max),
        ),
    }
}

/// How a design's points fall across the pieces of a region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Allocation {
    /// Pieces the reference visits and the design does not.
    pub missing: usize,
    /// The worst piece's shortfall against its share of the reference,
    /// `(n · share - held) / (n · share)`: `1.0` when a piece is missing,
    /// `0.0` when every piece holds at least its share, negative when every
    /// piece holds more than its share — which only a design of more than
    /// `n` points could, so in practice at most `0.0`.
    pub worst_shortfall: f64,
}

/// How `design_labels` — a piece per design point — fall across the pieces
/// `reference_labels` visits, one label per reference point.
///
/// The labels are the caller's: what counts as a piece of a region (a level,
/// a component, an arm of a disjunction) is a property of the fixture, not
/// something the measure should guess.
///
/// # Panics
/// If either list is empty, or a design point is labelled with a piece the
/// reference never visits.
#[must_use]
pub fn allocation(design_labels: &[usize], reference_labels: &[usize]) -> Allocation {
    assert!(
        !design_labels.is_empty() && !reference_labels.is_empty(),
        "an allocation needs a design and a reference"
    );
    let mut pieces: std::collections::BTreeMap<usize, (usize, usize)> =
        std::collections::BTreeMap::new();
    for label in reference_labels {
        pieces.entry(*label).or_default().1 += 1;
    }
    for label in design_labels {
        let piece = pieces.get_mut(label);
        assert!(
            piece.is_some(),
            "a design point is in piece {label}, which the reference never visits"
        );
        if let Some((held, _)) = piece {
            *held += 1;
        }
    }
    let count = design_labels.len() as f64;
    let total = reference_labels.len() as f64;
    Allocation {
        missing: pieces.values().filter(|(held, _)| *held == 0).count(),
        worst_shortfall: pieces
            .values()
            .map(|(held, visits)| {
                let expected = count * *visits as f64 / total;
                (expected - *held as f64) / expected
            })
            .fold(f64::NEG_INFINITY, f64::max),
    }
}

/// Every measure that needs nothing but the design and the reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Quality {
    pub cl2: Discrepancy,
    /// [`strata`], per variable.
    pub strata: Vec<Option<f64>>,
    /// [`level_shares`], per variable.
    pub level_shares: Vec<Option<f64>>,
    pub min_distance: f64,
    /// [`covering_radius`]: `None` against a uniform reference.
    pub covering_radius: Option<f64>,
}

/// [`cl2`], [`strata`], [`level_shares`], [`min_distance`] and
/// [`covering_radius`] of one design. [`allocation`] is not here because its
/// labels are the caller's.
///
/// # Panics
/// As [`cl2`].
#[must_use]
pub fn assess(reference: &Reference, design: MatRef<'_, f64>) -> Quality {
    Quality {
        cl2: cl2(reference, design),
        strata: strata(reference, design),
        level_shares: level_shares(reference, design),
        min_distance: min_distance(reference, design),
        covering_radius: covering_radius(reference, design),
    }
}

#[cfg(test)]
mod tests {
    use faer::Mat;

    use super::*;
    use crate::system::tests::system;
    use crate::{Domain, InputVariable};

    fn unit(names: &[&str]) -> ConstraintSystem {
        system(
            names
                .iter()
                .map(|name| InputVariable::new_continuous(*name, 0.0, 1.0))
                .collect(),
            &[],
        )
    }

    /// One column per point.
    fn design(points: &[&[f64]]) -> Mat<f64> {
        Mat::from_fn(points[0].len(), points.len(), |row, column| {
            points[column][row]
        })
    }

    /// The message of the panic `act` raises.
    fn panic_message(act: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let payload = std::panic::catch_unwind(act).expect_err("the call should panic");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .expect("a panic message")
    }

    /// `13/12 - 2·1 + 1·1`: the centre is where every kernel factor is one.
    #[test]
    fn when_one_point_sits_at_the_centre_of_a_line_should_have_cl2_squared_of_a_twelfth() {
        // setup
        let system = unit(&["x"]);
        let reference = Reference::uniform(&system);

        // act
        let discrepancy = cl2(&reference, design(&[&[0.5]]).as_ref());

        // assert
        assert!((discrepancy.squared - 1.0 / 12.0).abs() < 1e-15);
        // Random is `(5/4 - 13/12) / 1 = 1/6`.
        assert!((discrepancy.ratio - 0.5).abs() < 1e-14);
    }

    /// A midpoint grid is the uniform measure up to quadrature error, and the
    /// V-statistic's diagonal adds `(E k(Y,Y) - E k(Y,Y')) / m`; both are
    /// below `2/m` here.
    #[test]
    fn when_the_reference_is_a_fine_midpoint_grid_should_agree_with_the_closed_form() {
        for (names, side) in [(&["x"][..], 1000_usize), (&["x", "y"][..], 60)] {
            // setup
            let system = unit(names);
            let dims = names.len();
            let count = side.pow(dims as u32);
            let grid = Mat::from_fn(dims, count, |row, column| {
                let index = column / side.pow(row as u32) % side;
                (index as f64 + 0.5) / side as f64
            });
            let sampled = Reference::sample(&system, grid.as_ref());
            let uniform = Reference::uniform(&system);
            let points = design(&[
                &[0.2, 0.7][..dims],
                &[0.9, 0.1][..dims],
                &[0.4, 0.4][..dims],
            ]);

            // act
            let against_grid = cl2(&sampled, points.as_ref());
            let against_box = cl2(&uniform, points.as_ref());

            // assert
            assert!(
                (against_grid.squared - against_box.squared).abs() < 2.0 / count as f64,
                "{dims}D: {against_grid:?} against {against_box:?}"
            );
        }
    }

    /// Both are Latin; the diagonal is the identity permutation on both
    /// axes, every pair perfectly correlated, and the knight's-move square
    /// is not.
    #[test]
    fn when_a_design_is_the_diagonal_should_be_latin_on_every_axis_and_worse_than_a_latin_square_in_cl2()
     {
        // setup
        let reference = Reference::uniform(&unit(&["x", "y"]));
        let diagonal = design(&[
            &[0.125, 0.125],
            &[0.375, 0.375],
            &[0.625, 0.625],
            &[0.875, 0.875],
        ]);
        let square = design(&[
            &[0.125, 0.625],
            &[0.375, 0.125],
            &[0.625, 0.875],
            &[0.875, 0.375],
        ]);

        // act
        let diagonal_strata = strata(&reference, diagonal.as_ref());
        let square_strata = strata(&reference, square.as_ref());
        let diagonal_cl2 = cl2(&reference, diagonal.as_ref());
        let square_cl2 = cl2(&reference, square.as_ref());

        // assert
        assert_eq!(diagonal_strata, vec![Some(1.0), Some(1.0)]);
        assert_eq!(square_strata, vec![Some(1.0), Some(1.0)]);
        assert!(
            diagonal_cl2.squared > square_cl2.squared,
            "{diagonal_cl2:?} against {square_cl2:?}"
        );
    }

    #[test]
    fn when_two_points_share_a_stratum_should_score_n_minus_one_over_n() {
        // setup
        let reference = Reference::uniform(&unit(&["x"]));

        // act
        let held = strata(
            &reference,
            design(&[&[0.1], &[0.15], &[0.6], &[0.9]]).as_ref(),
        );

        // assert
        assert_eq!(held, vec![Some(0.75)]);
    }

    /// Strata are the reference's quantiles: against a sample bunched low,
    /// points that are evenly spaced in the box are not Latin.
    #[test]
    fn when_the_reference_is_skewed_should_stratify_by_its_quantiles() {
        // setup
        let system = unit(&["x"]);
        let bunched = Mat::from_fn(1, 8, |_, column| {
            [0.0, 0.01, 0.02, 0.03, 0.04, 0.05, 0.5, 1.0][column]
        });
        let reference = Reference::sample(&system, bunched.as_ref());

        // act
        let held = strata(
            &reference,
            design(&[&[0.125], &[0.375], &[0.625], &[0.875]]).as_ref(),
        );

        // assert
        // CDFs 6/8, 6/8, 7/8, 7/8: strata 3, 3, 3, 3.
        assert_eq!(held, vec![Some(0.25)]);
    }

    #[test]
    fn when_a_point_is_duplicated_should_have_min_distance_zero() {
        // setup
        let reference = Reference::uniform(&unit(&["x", "y"]));

        // act
        let nearest = min_distance(
            &reference,
            design(&[&[0.3, 0.3], &[0.9, 0.1], &[0.3, 0.3]]).as_ref(),
        );

        // assert
        assert_eq!(nearest, 0.0);
    }

    #[test]
    fn when_two_points_sit_at_opposite_corners_should_be_root_two_apart_whatever_the_box() {
        // setup
        let system = system(
            vec![
                InputVariable::new_continuous("x", 0.0, 2.0),
                InputVariable::new_continuous("y", 0.0, 4.0),
            ],
            &[],
        );
        let reference = Reference::uniform(&system);

        // act
        let nearest = min_distance(&reference, design(&[&[0.0, 0.0], &[2.0, 4.0]]).as_ref());

        // assert
        assert_eq!(nearest, 2.0_f64.sqrt());
    }

    #[test]
    fn when_the_design_is_the_centre_and_the_reference_the_corners_should_cover_at_root_half() {
        // setup
        let system = unit(&["x", "y"]);
        let corners = design(&[&[0.0, 0.0], &[0.0, 1.0], &[1.0, 0.0], &[1.0, 1.0]]);
        let reference = Reference::sample(&system, corners.as_ref());

        // act
        let radius = covering_radius(&reference, design(&[&[0.5, 0.5]]).as_ref());

        // assert
        assert_eq!(radius, Some(0.5_f64.sqrt()));
        assert_eq!(
            covering_radius(
                &Reference::uniform(&system),
                design(&[&[0.5, 0.5]]).as_ref()
            ),
            None
        );
    }

    /// Pieces at 3:1 in the reference; four points, all in the larger.
    #[test]
    fn when_a_piece_is_empty_should_count_it_missing() {
        // setup
        let reference_labels = [0, 0, 0, 1, 0, 0, 0, 1];

        // act
        let starved = allocation(&[0, 0, 0, 0], &reference_labels);
        let fair = allocation(&[0, 1, 0, 0], &reference_labels);

        // assert
        assert_eq!(
            starved,
            Allocation {
                missing: 1,
                worst_shortfall: 1.0
            }
        );
        assert_eq!(
            fair,
            Allocation {
                missing: 0,
                worst_shortfall: 0.0
            }
        );
    }

    /// Three levels against a uniform reference: two of four points on the
    /// first level is a share of a half against a third.
    #[test]
    fn when_a_level_is_overrepresented_should_report_its_excess_share() {
        // setup
        let system = system(
            vec![
                InputVariable::new("ratio", Domain::Levels(vec![1.0, 2.0, 4.0])),
                InputVariable::new_continuous("speed", 0.0, 1.0),
            ],
            &[],
        );
        let reference = Reference::uniform(&system);

        // act
        let shares = level_shares(
            &reference,
            design(&[&[1.0, 0.1], &[1.0, 0.4], &[2.0, 0.6], &[4.0, 0.9]]).as_ref(),
        );

        // assert
        assert_eq!(shares[1], None);
        assert!((shares[0].expect("ratio is discrete") - (0.5 - 1.0 / 3.0)).abs() < 1e-15);
    }

    /// Over levels the uniform reference's expectations are sums, and a
    /// sample of every level once is the same measure: the two must agree
    /// to rounding.
    #[test]
    fn when_a_lattice_is_sampled_level_by_level_should_agree_with_its_uniform_reference() {
        // setup
        let system = system(vec![InputVariable::new_discrete("n", 0.0, 1.0, 6.0)], &[]);
        let every_level = Mat::from_fn(1, 7, |_, column| column as f64);
        let points = design(&[&[1.0], &[5.0]]);

        // act
        let uniform = cl2(&Reference::uniform(&system), points.as_ref());
        let sampled = cl2(
            &Reference::sample(&system, every_level.as_ref()),
            points.as_ref(),
        );

        // assert
        assert!(
            (uniform.squared - sampled.squared).abs() < 1e-14,
            "{uniform:?} against {sampled:?}"
        );
        assert!(
            (uniform.ratio - sampled.ratio).abs() < 1e-12,
            "{uniform:?} against {sampled:?}"
        );
    }

    #[test]
    fn when_a_design_leaves_the_box_should_refuse_it_by_name() {
        // setup
        let reference = Reference::uniform(&unit(&["x"]));
        let outside = design(&[&[1.5]]);

        // act
        let message = panic_message(|| {
            let _ = cl2(&reference, outside.as_ref());
        });

        // assert
        assert_eq!(
            message,
            "the design has 1.5 at row 0 of column 0, outside the box"
        );
    }

    #[test]
    fn when_a_design_has_the_wrong_rows_should_refuse_it_by_name() {
        // setup
        let reference = Reference::uniform(&unit(&["x", "y"]));
        let short = design(&[&[0.5]]);

        // act
        let message = panic_message(|| {
            let _ = strata(&reference, short.as_ref());
        });

        // assert
        assert_eq!(message, "the design has one row per variable, 2, not 1");
    }
}

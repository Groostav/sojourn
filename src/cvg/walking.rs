//! Hit-and-run: walk the chord of the feasible region through the current point.
//!
//! From a feasible point, pick a direction uniformly on the unit sphere, take the
//! line through the point in that direction, and jump to a uniformly chosen
//! feasible place on it. Repeat. Needs no solver — it is arithmetic over
//! `evaluate` — but it needs a feasible point to start from.
//!
//! # Why this and not the JVM walker
//!
//! `RandomBoundedWalkingImproverPool` took a *half*-ray from a fixed base and
//! placed the result at `base + offset * nextDouble()` — uniform in radius.
//! Volume in `d` dimensions grows as `r^(d-1)`, so uniform-in-radius piles
//! points around the base and leaves the boundary empty. In 200 dimensions
//! essentially all the volume is near the boundary, so that is the whole region
//! missed.
//!
//! Hit-and-run (Smith, 1984) fixes it by construction rather than by correction:
//! take the chord both ways, sample uniformly along it, and make the result the
//! next state. The chain converges to the uniform distribution over the region.
//!
//! # Finding the chord: shrinkage, not bisection
//!
//! The obvious way to find where the region ends along the ray is to bisect
//! inward from the box wall. It is wrong, and instructively so. Binary search
//! finds the *first* boundary only when the predicate is monotone, and
//! feasibility along a line is not: for `(x+2)(x-1) == 0 +/- 1` the region is
//! two bands, and a ray from x = -2 toward the wall at x = 5 passes through the
//! gap and back into the far band, so the search brackets a chord spanning both.
//! Points sampled on it land in the gap, the move is refused, and the chain sits
//! still emitting the same point.
//!
//! Neal's shrinkage procedure (slice sampling, 2003) is the fix, and is cheaper
//! than what it replaces. Start with the whole box chord, sample on it, and if
//! the draw is infeasible shrink that side of the interval *to the draw* and try
//! again. It needs no monotonicity, converges geometrically onto the feasible
//! piece holding the current point, costs one feasibility test per attempt
//! rather than two brackets' worth, and leaves the uniform distribution
//! invariant.
//!
//! # Two kinds of move
//!
//! Pure hit-and-run mixes slowly in high dimensions, and measurably so: in the
//! 200-variable benchmark a move travels about 0.035 through a region 7.07
//! across, because the chord is cut short by the nearest of two hundred walls.
//! That is a random walk needing roughly `(7.07/0.035)^2` — about 40,000 —
//! moves per independent sample, which is the textbook `O(d^2)` and not
//! something a constant factor fixes.
//!
//! So half the moves step along a coordinate axis instead, sweeping the
//! coordinates in order. On a region whose bounds are axis-aligned an axis move
//! resamples that coordinate across its whole feasible range, so one sweep
//! produces an independent point and mixing becomes `O(d)`. Axis moves are
//! useless on a region angled across the coordinates — a diagonal ribbon leaves
//! nowhere to go — which is exactly where the random directions carry the
//! chain, so the two cover each other.
//!
//! Mixing them is sound: both kernels leave the uniform distribution invariant
//! (an axis move is Gibbs on that coordinate), and a random choice between
//! kernels sharing a stationary distribution preserves it.
//!
//! It also means a chain is not confined to its starting component: a first draw
//! landing in another piece is feasible, so it is taken. The proposal is
//! symmetric — the box chord is the same segment from either end — so this is a
//! legitimate move rather than a leak. Crossing still needs both pieces to share
//! a line, which is rare enough that a seed in each remains worth having.
//!
//! # Preconditioning: directions drawn from the region's own shape
//!
//! Random directions on the sphere are the right answer for a round region and
//! the wrong one for a tube. P118's polytope couples its variables in chains
//! of ±7 bands, so it is long along a few diagonals and thin across every
//! other direction; a uniformly random direction almost always points at a
//! nearby wall, chords are short, and the chain crawls along the tube. Measured:
//! at two moves per dimension between emissions it fails its two-run agreement
//! on four seeds in ten, and at twenty it passes all ten. The walker converges;
//! it emits ten times too often for that shape. Raising the thinning for every
//! problem would multiply the cost of the 200-dimensional cases, which are round
//! and mix fine as they are.
//!
//! So the random half of the moves is drawn from the shape of the region as the
//! chains learnt it during burn-in: a standard normal draw is multiplied by the
//! Cholesky factor of the states' covariance and normalised, which makes a
//! direction along the tube's long axis as likely as the tube is long. Axis
//! moves are untouched — on an axis-aligned box they are exact Gibbs steps and
//! rotating them would only hurt.
//!
//! **This is a change of coordinates, not a bias.** Hit-and-run with any
//! direction distribution that is symmetric (`d` and `-d` equally likely) and
//! reaches every direction leaves the uniform distribution invariant (Bélisle,
//! Romeijn and Smith, 1993); the shape only decides how fast the chain mixes.
//! A badly estimated shape therefore costs speed and never correctness, which is
//! what makes it safe to estimate from a burn-in that has not itself mixed.
//!
//! **The estimate is frozen before anything is emitted.** Refitting as the run
//! proceeds would make the chain non-Markovian and void the argument above. It
//! is fitted from the second half of every chain's burn-in, refined by a second
//! burn-in walked under it — chains that crawled on the sphere never traversed
//! the tube, so the first fit under-reads its length, and on P118 the first fit
//! alone left one seed in ten marginally red — and then kept for the walker's
//! life.
//!
//! **The estimate is shrunk toward its diagonal by `m / n`.** Burn-in states
//! are heavily autocorrelated — a coordinate is only refreshed by its own axis
//! move, every `2d` steps — so a 200-dimensional covariance is estimated from
//! a few dozen effective samples and its spectrum is mostly sampling noise: on a
//! round box that noise would *introduce* anisotropy where the truth has none.
//! `m / n`, dimensions over samples, is the ratio that governs that spread, and
//! blending toward the diagonal with that weight bounds the noise's condition
//! number at about four for any ratio below one and collapses to the diagonal
//! at one or more. In 200 dimensions that is the diagonal, which is harmless
//! on a round box; on P118, with fifteen dimensions and a few hundred samples,
//! it is nearly the full covariance, and the tube's axis is a large eigenvalue
//! that a few dozen samples already resolve.
//!
//! The fit is deterministic per seed on one machine. Across machines `faer`'s
//! matrix product may sum in a different order, so the factor can differ in
//! the last place and the emitted points with it; nothing depends on the same
//! seed reproducing across machines.

use faer::{Mat, Side};
use rand::RngExt;
use rand::rngs::Xoshiro256PlusPlus;

use super::support::{Levels, VariableSupport};
use super::{classify, hc4};
use crate::{ConstraintSystem, Point};

/// How many chains to run at once.
///
/// Emission is round-robin across them, which is the cheap way to decorrelate:
/// consecutive returned points come from different chains. It is also the only
/// way a region in several pieces gets covered, since one chain rarely crosses
/// between them — so this wants to stay comfortably above the number of pieces
/// any problem has.
///
/// Kept modest because every chain pays burn-in, and burn-in is the walker's
/// dominant cost. Decorrelation is mostly [`THINNING_PER_DIMENSION`]'s job.
const CHAIN_COUNT: usize = 8;

/// Steps to discard per dimension when a chain starts, so its output stops
/// depending on where it was seeded.
///
/// Scaled by dimension because mixing is: one move changes the point along a
/// single line, so a chain needs at least on the order of `d` moves before it
/// has explored `d` independent directions. A fixed burn-in that is ample in one
/// dimension leaves a 200-dimensional chain sitting next to its seed.
const BURN_IN_PER_DIMENSION: usize = 16;

/// Floor for the above.
///
/// High for a floor, because dimension is not the only thing that makes a chain
/// slow. P118's polytope is narrow and its constraints couple the variables in
/// pairs, so the scaled figure came to 240 steps at fifteen dimensions and left
/// the eight chains still clustered around wherever they had started — their
/// per-coordinate centroids disagreeing by four times the sampling error.
///
/// Raising it did *not*, on its own, make the benchmark's two-run comparison
/// agree; that turned out to be a flaw in the comparison rather than in the
/// walker. It is kept because 240 steps is too few on the evidence above,
/// not because it fixed a test.
const MINIMUM_BURN_IN: usize = 2_000;

/// Steps taken between emitted points, per dimension.
///
/// Scaled for the same reason as burn-in, and it is the more important of the
/// two. A move changes the point along one line, so after two moves a
/// 200-dimensional point has been perturbed in two of its two hundred degrees of
/// freedom and is very nearly the point it was. Emitting on that cadence
/// produces a sample that looks converged per-chain and is in fact a handful of
/// points wearing two hundred hats.
///
/// Two per dimension because only half the moves are axis moves, so that is what
/// it takes to complete one sweep of the coordinates.
const THINNING_PER_DIMENSION: usize = 2;

/// How often a move steps along a coordinate axis rather than a random
/// direction. Half and half: see the module documentation for why neither alone
/// is enough.
const AXIS_MOVE_PROBABILITY: f64 = 0.5;

/// Floor for the above, so one-dimensional problems still take a step or two
/// between emissions.
const MINIMUM_THINNING: usize = 2;

/// How many times a move may shrink its interval before giving up.
///
/// Each shrink cuts the interval roughly in half toward the current point, so
/// this is a budget in bits: 64 is past the point where an `f64` interval can
/// still be halved meaningfully. A move that exhausts it stays put.
const SHRINK_LIMIT: usize = 64;

#[derive(Clone)]
pub(crate) struct HitAndRunWalker {
    rng: Xoshiro256PlusPlus,
    chains: Vec<Chain>,
    frame: Frame,
    /// Candidates judged so far, over every move: the walker's cost in the
    /// unit it is paid in, reported at `debug` with each phase.
    judged: u64,
}

/// What a move ranges over: the coordinates, fixed for the problem when the
/// chains are started, and the shape learnt over them. Every move reads it
/// and none writes it; burn-in is the only writer.
#[derive(Clone)]
struct Frame {
    /// The coordinates worth moving: every coordinate unless some are
    /// driven, in which case moving those directly walks off the surface that
    /// defines them and retraction overwrites the move anyway. The union over
    /// the problem's plans; a chain moves only its own plan's free
    /// coordinates, and the shape is fitted over the union.
    movable: Vec<usize>,
    /// The continuous ones among `movable`: what a random direction has
    /// components in and the shape is fitted over. A discrete coordinate
    /// moves only along its own axis, from level to level — a direction
    /// through it would land between levels on every draw.
    continuous: Vec<usize>,
    /// The region's shape, fitted once the chains have burnt in; `None` until
    /// then, and afterwards if there was nothing to fit or it would not factor.
    transform: Option<Preconditioner>,
}

/// The shape of the region as the chains learnt it: the lower Cholesky factor
/// of the covariance of their burn-in states, over the movable coordinates,
/// shrunk toward its diagonal. See the module documentation for why this is
/// sound whatever it estimates, and why it is frozen.
#[derive(Clone)]
struct Preconditioner {
    /// `L`, with `L Lᵀ` the shrunk covariance; zero above the diagonal.
    factor: Mat<f64>,
}

impl Preconditioner {
    /// Smallest variance any coordinate is credited with, as a fraction of the
    /// largest. A coordinate that never moved during burn-in would otherwise
    /// factor as exactly zero and be frozen out of every random move; at one
    /// percent of scale it stays reachable, and its axis moves cover it anyway.
    const DIAGONAL_FLOOR: f64 = 1e-4;

    /// Fits the shape from `states`, each a point projected onto the movable
    /// coordinates. `None` when there is nothing to fit: no coordinates, fewer
    /// than two states, or a covariance that will not factor.
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample and dimension counts are far below 2^53"
    )]
    fn fit(states: &[Vec<f64>]) -> Option<Self> {
        let n = states.len();
        let m = states.first()?.len();
        if n < 2 || m == 0 {
            return None;
        }

        let mean: Vec<f64> = (0..m)
            .map(|j| states.iter().map(|state| state[j]).sum::<f64>() / n as f64)
            .collect();
        let centred = Mat::from_fn(n, m, |i, j| states[i][j] - mean[j]);
        let scale = 1.0 / (n - 1) as f64;
        let covariance = centred.transpose() * &centred;

        // Shrinkage toward the diagonal by the dimensions-to-samples ratio;
        // the module documentation says why that ratio and not a constant.
        let alpha = (m as f64 / n as f64).min(1.0);
        let largest = (0..m)
            .map(|j| covariance[(j, j)] * scale)
            .fold(0.0_f64, f64::max);
        let floor = if largest > 0.0 {
            largest * Self::DIAGONAL_FLOOR
        } else {
            1.0
        };
        let shrunk = Mat::from_fn(m, m, |i, j| {
            let value = covariance[(i, j)] * scale;
            if i == j {
                value.max(floor)
            } else {
                (1.0 - alpha) * value
            }
        });

        match shrunk.llt(Side::Lower) {
            Ok(llt) => Some(Self {
                factor: llt.L().to_owned(),
            }),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    samples = n,
                    dimensions = m,
                    "the region's shape would not factor; directions stay on the sphere"
                );
                None
            }
        }
    }

    /// `unit`, a direction on the sphere, bent to the region's shape:
    /// `L · unit`, normalised. Odd in `unit`, so the direction distribution
    /// stays symmetric.
    fn direction(&self, unit: &[f64]) -> Vec<f64> {
        let m = unit.len();
        let mut bent = vec![0.0; m];
        // Column-major over the lower triangle: `L` is zero above the diagonal,
        // so column `j` contributes to rows `j..` only.
        for (j, &z) in unit.iter().enumerate() {
            let column = self.factor.col_as_slice(j);
            for (out, &entry) in bent[j..].iter_mut().zip(&column[j..]) {
                *out += entry * z;
            }
        }
        let norm = bent.iter().map(|c| c * c).sum::<f64>().sqrt();
        if norm > 0.0 && norm.is_finite() {
            for component in &mut bent {
                *component /= norm;
            }
            bent
        } else {
            unit.to_vec()
        }
    }
}

/// Moves taken between emitted points, and between the burn-in states the
/// shape is fitted from.
fn thinning_for(dimensions: usize) -> usize {
    MINIMUM_THINNING.max(THINNING_PER_DIMENSION * dimensions)
}

/// A chain's current position, and how far through the coordinate sweep it is.
///
/// The cursor is per chain rather than global so that each chain sweeps every
/// coordinate; sharing one would let chains interleave and leave coordinates
/// untouched.
#[derive(Clone)]
struct Chain {
    point: Point,
    steps: usize,
    /// The plan this chain moves under, chosen where it started as the one
    /// that pinned its start hardest — on a disjunction, the arm it is on.
    /// An index into the problem's plans; `None` where nothing is driven.
    plan: Option<usize>,
}

impl HitAndRunWalker {
    pub(crate) const fn new(rng: Xoshiro256PlusPlus) -> Self {
        Self {
            rng,
            chains: Vec::new(),
            frame: Frame {
                movable: Vec::new(),
                continuous: Vec::new(),
                transform: None,
            },
            judged: 0,
        }
    }

    /// This walker — its chains where they stand, its shape as learnt —
    /// drawing from `rng` from here on: what a design walks with, from a
    /// clone of the region's burnt-in walker, under the design's own seed.
    pub(crate) fn reseeded(self, rng: Xoshiro256PlusPlus) -> Self {
        Self { rng, ..self }
    }

    /// Starts any chains that do not exist yet, spread as widely across what has
    /// been found as possible.
    ///
    /// A region in several pieces is only covered if the chains start in several
    /// pieces, and a chain cannot cross between them afterwards — so where the
    /// chains begin is the whole of the coverage question, not a detail.
    ///
    /// **Picking at random does not achieve that, and used to.** By the time
    /// chains are started, `existing` is dominated by whatever the search could
    /// already reach: a batch of points walked out from the first seed, plus
    /// perhaps a single seed in the piece nothing had reached. Drawing eight
    /// times uniformly from thirty-three points of which one is the interesting
    /// one misses it about four times in five, which is precisely what
    /// `both_bands_of_a_parabola_receive_points` measured.
    ///
    /// So the first half of the chains are placed by farthest-point selection:
    /// take one at random, then repeatedly take whichever candidate is farthest
    /// from everything already taken. The rare seed in the far piece is not one
    /// point among many, it is the farthest point there is, and it gets chosen
    /// second.
    ///
    /// **Only half**, because placing every chain that way is its own bias.
    /// Farthest-point on a connected box puts chains in its corners, and the
    /// burn-in does not wash that out: doing it for all eight cost
    /// `top_corner_200d` its uniformity oracle at KS 0.1683 against 0.1628.
    /// Coverage needs one chain per component and the corpus has two, so four
    /// is generous insurance; the rest are drawn from the bulk, unbiased, as
    /// they always were.
    ///
    /// Burn-in is also where the region's shape is learnt: the second half of
    /// every chain's burn-in, sampled at the emission cadence, is pooled and
    /// fitted once the last chain is in. That burn-in runs on the sphere; a
    /// second one runs under the estimate and refits it, for the reason given
    /// where it happens.
    ///
    /// A function of the region — its points and this walker's stream — and
    /// of nothing a design asks for, which is why `solve` pays it once and
    /// the region keeps the chains; [`extend`](Self::extend) still runs it
    /// where nothing has. Nothing to do once the chains are placed.
    pub(crate) fn burn_in(&mut self, existing: &[Point], problem: &ConstraintSystem) {
        if self.chains.len() >= CHAIN_COUNT {
            return;
        }
        let dimensions = existing[0].len();
        let burn_in = MINIMUM_BURN_IN.max(BURN_IN_PER_DIMENSION * dimensions);
        let cadence = thinning_for(dimensions);
        self.frame.movable = if problem.plans.is_empty() {
            (0..dimensions).collect()
        } else {
            let mut union: Vec<usize> = problem
                .plans
                .iter()
                .flat_map(|plan| plan.free().iter().copied())
                .collect();
            union.sort_unstable();
            union.dedup();
            union
        };
        self.frame.continuous = self
            .frame
            .movable
            .iter()
            .copied()
            .filter(|slot| problem.supports[*slot].levels().is_none())
            .collect();
        let mut states: Vec<Vec<f64>> = Vec::new();

        // Selection is quadratic in the candidate count, and `existing` grows
        // without bound as a search runs. A random window keeps the cost fixed;
        // it also keeps recent points in play rather than freezing on the first
        // batch forever.
        let candidates: Vec<&Point> = if existing.len() <= SELECTION_WINDOW {
            existing.iter().collect()
        } else {
            (0..SELECTION_WINDOW)
                .map(|_| &existing[self.rng.random_range(0..existing.len())])
                .collect()
        };

        let _span =
            tracing::debug_span!("burn_in", chains = CHAIN_COUNT, steps = burn_in).entered();
        let mut chosen: Vec<&Point> = Vec::new();
        while self.chains.len() < CHAIN_COUNT {
            let spread = chosen.len() < CHAIN_COUNT / 2;
            let start = if chosen.is_empty() || !spread {
                candidates[self.rng.random_range(0..candidates.len())]
            } else {
                // Farthest from everything taken so far. `max_by` on a partial
                // order needs a total one; `total_cmp` gives it, and a NaN
                // distance simply sorts low rather than panicking.
                candidates
                    .iter()
                    .copied()
                    .max_by(|left, right| {
                        nearest_distance(left, &chosen).total_cmp(&nearest_distance(right, &chosen))
                    })
                    .unwrap_or(candidates[0])
            };
            chosen.push(start);

            // The plan that pins the start hardest; where several tie — the
            // start is on every branch at once, a crossing — the chains
            // seeded there are spread across them.
            let tied = classify::tightest(problem, start);
            let mut chain = Chain {
                point: start.clone(),
                steps: 0,
                plan: (!tied.is_empty()).then(|| tied[self.chains.len() % tied.len()]),
            };
            for step in 0..burn_in {
                let (moved, judged) = advance(
                    chain.point,
                    step,
                    &mut self.rng,
                    problem,
                    chain.plan.map(|plan| &problem.plans[plan]),
                    &self.frame,
                );
                chain.point = moved;
                self.judged += judged;
                if step >= burn_in / 2 && step % cadence == 0 {
                    states.push(
                        self.frame
                            .continuous
                            .iter()
                            .map(|&slot| chain.point[slot])
                            .collect(),
                    );
                }
            }
            chain.steps = burn_in;
            self.chains.push(chain);
        }
        tracing::debug!(judged = self.judged, "burnt in on the sphere");

        self.frame.transform = Preconditioner::fit(&states);

        // A second burn-in under that estimate, and a second fit. Chains that
        // crawled on the sphere never traversed a tube, so the first estimate
        // under-reads its length; chains that walk under it do, and the refit
        // reads the shape the emission will actually see. Refitting before
        // anything is emitted keeps the emitted chain Markov. Skipped when the
        // first fit had fewer states than dimensions: its shrinkage collapsed
        // it to the diagonal, a refit from the same counts would collapse the
        // same way, and the second burn-in would buy nothing at 200 dimensions.
        if self.frame.transform.is_some() && states.len() > self.frame.continuous.len() {
            let mut states: Vec<Vec<f64>> = Vec::new();
            for chain in &mut self.chains {
                for _ in 0..burn_in {
                    let (moved, judged) = advance(
                        std::mem::take(&mut chain.point),
                        chain.steps,
                        &mut self.rng,
                        problem,
                        chain.plan.map(|plan| &problem.plans[plan]),
                        &self.frame,
                    );
                    chain.point = moved;
                    self.judged += judged;
                    chain.steps += 1;
                    if chain.steps % cadence == 0 {
                        states.push(
                            self.frame
                                .continuous
                                .iter()
                                .map(|&slot| chain.point[slot])
                                .collect(),
                        );
                    }
                }
            }
            tracing::debug!(judged = self.judged, "burnt in under the shape");
            if let Some(refit) = Preconditioner::fit(&states) {
                self.frame.transform = Some(refit);
            }
        }
    }
}

impl HitAndRunWalker {
    /// Up to `count` more points, walked out from the chains — started, if
    /// they have not been, from points chosen at random across `from`.
    ///
    /// Every point returned is feasible by the chain's invariant; the pool
    /// judges them again anyway, because "never an infeasible one" is its
    /// promise and not this function's. Nothing to walk from is not an error:
    /// on a tight region it is the normal state until a seed exists.
    pub(crate) fn extend(
        &mut self,
        problem: &ConstraintSystem,
        from: &[Point],
        count: usize,
    ) -> Vec<Point> {
        if count == 0 || from.is_empty() {
            return Vec::new();
        }
        self.burn_in(from, problem);
        if self.chains.is_empty() {
            return Vec::new();
        }
        let thinning = thinning_for(from[0].len());
        let _span = tracing::debug_span!("walk", count, thinning).entered();
        let before = self.judged;

        // Sequential by nature — a chain cannot take its next step until it has
        // judged this one — so the points are walked one at a time.
        let points: Vec<Point> = (0..count)
            .map(|emitted| {
                let index = emitted % self.chains.len();
                let mut point = std::mem::take(&mut self.chains[index].point);
                let mut steps = self.chains[index].steps;
                let plan = self.chains[index].plan.map(|plan| &problem.plans[plan]);
                for _ in 0..thinning {
                    let (moved, judged) =
                        advance(point, steps, &mut self.rng, problem, plan, &self.frame);
                    point = moved;
                    self.judged += judged;
                    steps += 1;
                }
                self.chains[index].point = point.clone();
                self.chains[index].steps = steps;
                point
            })
            .collect();
        tracing::debug!(judged = self.judged - before, "walked");
        points
    }
}

/// How many candidates farthest-point selection considers.
///
/// Selection is `CHAIN_COUNT` passes over this, so it is a fixed cost rather than
/// one that grows with a long-running search.
const SELECTION_WINDOW: usize = 256;

/// Distance from `point` to the nearest of `chosen`, squared.
///
/// Squared because only the ordering is used and a square root would change
/// nothing about it.
fn nearest_distance(point: &Point, chosen: &[&Point]) -> f64 {
    chosen
        .iter()
        .map(|other| {
            point
                .iter()
                .zip(other.iter())
                .map(|(a, b)| (a - b) * (a - b))
                .sum::<f64>()
        })
        .fold(f64::INFINITY, f64::min)
}

/// One hit-and-run move: sample the line through `from`, shrinking the interval
/// until the draw is feasible.
///
/// Returns `from` unchanged when there is nowhere to go — a degenerate box, or an
/// interval that collapsed before finding anything. A chain that stalls shows up
/// downstream as duplicate points rather than as a wrong answer, which is why the
/// benchmark harness checks for them.
fn advance(
    from: Point,
    step: usize,
    rng: &mut Xoshiro256PlusPlus,
    problem: &ConstraintSystem,
    plan: Option<&classify::Plan>,
    frame: &Frame,
) -> (Point, u64) {
    let dimensions = from.len();
    let mut judged = 0;
    // The chain's own free coordinates: `movable` where there is one plan or
    // none, a subset of it where the chain is on one branch of several.
    let free: &[usize] = plan.map_or(&frame.movable, classify::Plan::free);

    if free.is_empty() {
        // Every coordinate is driven, so there is no chord to draw — but the
        // bands still have width, and a Gibbs sweep over them is a legitimate
        // move. `TopCorner200DAsEqualities` is entirely this case.
        let mut candidate = from.clone();
        if let Some(plan) = plan {
            classify::retract(problem, plan, &mut candidate, rng);
        }
        let moved = if problem.is_feasible(&candidate, 0.0) {
            candidate
        } else {
            from
        };
        return (moved, 1);
    }

    // Swept in order rather than picked at random: a random scan needs
    // `d ln d` moves to touch every coordinate, a sweep needs `d`.
    let swept = free[step % free.len()];
    let along_axis = rng.random_range(0.0..1.0) < AXIS_MOVE_PROBABILITY;

    // A discrete coordinate moves along its axis whatever the coin said —
    // drawn every step all the same, so a system with no discrete
    // coordinate draws exactly what it always did.
    match &problem.supports[swept] {
        VariableSupport::Discrete(levels) => {
            return level_move(from, swept, levels, rng, problem, plan);
        }
        VariableSupport::Continuous { .. } => {}
    }

    let direction = if along_axis {
        let mut axis = vec![0.0; dimensions];
        axis[swept] = 1.0;
        axis
    } else {
        // On the sphere until the shape is known, then bent to it. Bending a
        // unit vector and normalising is bending the raw draw and normalising,
        // so the sphere draw is the same draw either way. A coordinate the
        // chain's plan drives gets no component: the move is in the plan's
        // free coordinates, and retraction would overwrite it anyway. Only
        // the continuous coordinates get one — the frame's `continuous`,
        // which the shape was fitted over — and the swept coordinate is one
        // of them, so the direction is never all zero.
        debug_assert!(
            frame.continuous.contains(&swept),
            "a continuous coordinate was swept, so there is one"
        );
        let unit = random_direction(rng, frame.continuous.len());
        let shaped = match &frame.transform {
            Some(transform) => transform.direction(&unit),
            None => unit,
        };
        let mut direction = vec![0.0; dimensions];
        for (slot, component) in frame.continuous.iter().zip(shaped) {
            if plan.is_none_or(|plan| !plan.drives(*slot)) {
                direction[*slot] = component;
            }
        }
        direction
    };

    // An axis move is a move in one coordinate, which is the one question the
    // constraints can be asked directly: `hc4::slice` propagates them and
    // answers with the interval this coordinate may occupy. A random direction
    // has no such answer — narrowing works per coordinate — so it still clips
    // against the box alone and finds feasibility by shrinking.
    let (mut lower, mut upper) = if along_axis {
        axis_chord(&from, swept, problem)
    } else {
        box_chord(&from, &direction, problem)
    };

    for _ in 0..SHRINK_LIMIT {
        if lower >= upper {
            break;
        }
        let step = rng.random_range(lower..=upper);
        let mut candidate: Point = from
            .iter()
            .zip(&direction)
            .map(|(value, component)| value + step * component)
            .collect();
        // Back onto the surface. Nothing to do when nothing is driven, and
        // never trusted: the judgement below is unchanged in what it concludes.
        if let Some(plan) = plan {
            classify::retract(problem, plan, &mut candidate, rng);
        }

        judged += 1;
        if problem.is_feasible(&candidate, 0.0) {
            return (candidate, judged);
        }

        // Shrink toward `from`, which is feasible by the chain's invariant, so
        // the interval always still contains a feasible point. `step` cannot be
        // zero here: zero is `from` itself and would have been accepted.
        if step < 0.0 {
            lower = step;
        } else {
            upper = step;
        }
    }
    (from, judged)
}

/// A point uniformly distributed on the unit sphere.
///
/// Gaussian components divided by the L2 norm — Muller's method. The JVM version
/// used uniform components over a cube, which biases directions toward the
/// cube's corners; it also divided by `abs(sum(components))` rather than the
/// norm, though that error cancelled, since the caller rescaled the direction to
/// the box wall and that rescaling is scale-invariant.
fn random_direction(rng: &mut Xoshiro256PlusPlus, dimensions: usize) -> Vec<f64> {
    loop {
        let components: Vec<f64> = (0..dimensions)
            .map(|_| {
                // Box-Muller. `random_range` is half-open at the top, so
                // `1.0 - u` keeps the logarithm away from zero.
                let u1: f64 = 1.0 - rng.random_range(0.0..1.0);
                let u2: f64 = rng.random_range(0.0..1.0);
                (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
            })
            .collect();

        let norm = components.iter().map(|c| c * c).sum::<f64>().sqrt();
        if norm > 0.0 && norm.is_finite() {
            return components.iter().map(|c| c / norm).collect();
        }
        // All components underflowed to zero. Vanishingly rare, and cheaper to
        // redraw than to reason about.
    }
}

/// The steps along one axis the constraints allow, as offsets from `from`.
///
/// The same shape [`box_chord`] returns, so the shrink loop above is unchanged
/// — but where that clips against the declared box and lets rejection find the
/// rest, this starts from what the constraints actually permit. On a tight
/// equality that is the difference between a chord spanning the whole box and
/// one spanning the tolerance.
///
/// The interval is a superset of the feasible slice, so the feasibility check
/// after each draw is still doing the deciding and this is still only a
/// proposal.
fn axis_chord(from: &Point, axis: usize, problem: &ConstraintSystem) -> (f64, f64) {
    let slice = hc4::slice(problem, from, axis);
    if slice.is_empty() {
        return (0.0, 0.0);
    }
    // `from` is feasible, so its own coordinate satisfies every constraint and
    // lies in the slice — but a point resting on a boundary can land a rounding
    // error outside it, exactly as `box_chord` guards for.
    (
        (slice.lo() - from[axis]).min(0.0),
        (slice.hi() - from[axis]).max(0.0),
    )
}

/// A Gibbs move on a discrete coordinate: a level drawn uniformly from those
/// the constraints leave it, the driven coordinates put back on their
/// surfaces, and the candidate judged — shrinking the range of levels toward
/// the current one on a miss, as [`advance`] shrinks a chord.
///
/// The slice is taken with the chain's driven coordinates *unsettled*: they
/// are recomputed after the move, so a constraint naming one says nothing
/// about where this coordinate may go. Conditioning on it would pin a count
/// of bolts to the load it was driven from, and the chain would never change
/// its count. The shrink excludes each rejected level, so none is drawn
/// twice; the current level, which the chain stands on, is never excluded,
/// and drawing it again re-draws the driven coordinates, which is a move.
fn level_move(
    from: Point,
    axis: usize,
    levels: &Levels,
    rng: &mut Xoshiro256PlusPlus,
    problem: &ConstraintSystem,
    plan: Option<&classify::Plan>,
) -> (Point, u64) {
    let slice = match plan {
        Some(plan) => {
            let mut settled = vec![true; from.len()];
            for driven in plan.driven() {
                settled[*driven] = false;
            }
            hc4::slice_conditioned(problem, &from, axis, Some(&settled))
        }
        None => hc4::slice(problem, &from, axis),
    };
    let (Some(range), Some(current)) = (levels.within(slice), levels.index_of(from[axis])) else {
        return (from, 0);
    };
    if !range.contains(&current) {
        return (from, 0);
    }

    let (mut lowest, mut highest) = (*range.start(), *range.end());
    let mut judged = 0;
    for _ in 0..SHRINK_LIMIT {
        let k = rng.random_range(lowest..=highest);
        let mut candidate = from.clone();
        candidate[axis] = levels.level(k);
        if let Some(plan) = plan {
            classify::retract(problem, plan, &mut candidate, rng);
        }
        judged += 1;
        if problem.is_feasible(&candidate, 0.0) {
            return (candidate, judged);
        }
        match k.cmp(&current) {
            std::cmp::Ordering::Less => lowest = k + 1,
            std::cmp::Ordering::Greater => highest = k - 1,
            std::cmp::Ordering::Equal => {}
        }
    }
    (from, judged)
}

/// How far `from` can travel either side along `direction` and stay in the box.
///
/// The interval brackets zero. This is a pure box calculation — feasibility does
/// not enter into it, because shrinkage is what handles the constraints. Its
/// counterpart [`axis_chord`] does ask them, which it can because a single
/// coordinate is a question narrowing can answer and an arbitrary direction is
/// not.
fn box_chord(from: &Point, direction: &[f64], problem: &ConstraintSystem) -> (f64, f64) {
    let (mut lower, mut upper) = (f64::NEG_INFINITY, f64::INFINITY);
    for (index, support) in problem.supports.iter().enumerate() {
        let component = direction[index];

        // A zero component means the ray is parallel to this pair of walls and
        // never meets them. Skipping is not just an optimisation: the JVM version
        // divided unguarded, and a point sitting exactly on a bound gives
        // 0.0/0.0, which Java's `Math.min` propagates as NaN and Rust's
        // `f64::min` silently discards. Two different wrong answers.
        if component == 0.0 {
            continue;
        }

        let to_lower = (support.lower() - from[index]) / component;
        let to_upper = (support.upper() - from[index]) / component;
        let (near, far) = if to_lower < to_upper {
            (to_lower, to_upper)
        } else {
            (to_upper, to_lower)
        };
        lower = lower.max(near);
        upper = upper.min(far);
    }

    // `from` is inside the box, so the interval contains zero — but a point
    // resting exactly on a bound can put it a rounding error the wrong side.
    (lower.min(0.0), upper.max(0.0))
}

#[cfg(test)]
mod tests {
    use rand::RngExt;
    use rand::SeedableRng;
    use rand::rngs::Xoshiro256PlusPlus;

    use super::{Preconditioner, random_direction};

    fn rng() -> Xoshiro256PlusPlus {
        Xoshiro256PlusPlus::seed_from_u64(0x5A_5A_5A_5A)
    }

    /// One standard normal draw, by Box-Muller.
    fn gaussian(rng: &mut Xoshiro256PlusPlus) -> f64 {
        let u1: f64 = 1.0 - rng.random_range(0.0..1.0);
        let u2: f64 = rng.random_range(0.0..1.0);
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// `n` draws from the Gaussian with covariance `L Lᵀ`.
    fn gaussian_states(rng: &mut Xoshiro256PlusPlus, factor: &[&[f64]], n: usize) -> Vec<Vec<f64>> {
        let m = factor.len();
        (0..n)
            .map(|_| {
                let z: Vec<f64> = (0..m).map(|_| gaussian(rng)).collect();
                factor
                    .iter()
                    .map(|row| row.iter().zip(&z).map(|(l, z)| l * z).sum())
                    .collect()
            })
            .collect()
    }

    /// `L Lᵀ` of a fitted factor.
    fn covariance_of(fitted: &Preconditioner) -> Vec<Vec<f64>> {
        let m = fitted.factor.nrows();
        (0..m)
            .map(|i| {
                (0..m)
                    .map(|j| {
                        (0..m)
                            .map(|k| fitted.factor[(i, k)] * fitted.factor[(j, k)])
                            .sum()
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_fit_reproduces_a_known_covariance() {
        // `L = [[2, 0], [0.6, 0.8]]`, so the covariance is `[[4, 1.2], [1.2, 1]]`.
        // Twenty thousand samples for two dimensions leaves the shrinkage
        // negligible and the sampling error a few percent.
        let states = gaussian_states(&mut rng(), &[&[2.0, 0.0], &[0.6, 0.8]], 20_000);
        let fitted = Preconditioner::fit(&states).expect("a full-rank sample fits");
        let covariance = covariance_of(&fitted);
        for (row, expected) in covariance.iter().zip([[4.0, 1.2], [1.2, 1.0]]) {
            for (got, want) in row.iter().zip(expected) {
                assert!(
                    (got - want).abs() < 0.1,
                    "{covariance:?} should be near [[4, 1.2], [1.2, 1]]"
                );
            }
        }
    }

    #[test]
    fn isotropic_samples_give_a_scaled_identity() {
        let identity: [&[f64]; 3] = [&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0], &[0.0, 0.0, 1.0]];
        let states = gaussian_states(&mut rng(), &identity, 20_000);
        let covariance = covariance_of(&Preconditioner::fit(&states).expect("fits"));
        for (i, row) in covariance.iter().enumerate() {
            for (j, value) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (value - want).abs() < 0.05,
                    "{covariance:?} should be near the identity"
                );
            }
        }
    }

    /// Fewer samples than dimensions is the regime where a raw covariance is
    /// mostly noise; the shrinkage weight reaches one and the factor is exactly
    /// diagonal, so the walker scales coordinates and rotates nothing.
    #[test]
    fn fewer_samples_than_dimensions_shrinks_to_the_diagonal() {
        let mut rng = rng();
        let states: Vec<Vec<f64>> = (0..5)
            .map(|_| (0..10).map(|_| gaussian(&mut rng)).collect())
            .collect();
        let fitted = Preconditioner::fit(&states).expect("the diagonal always factors");
        for i in 0..10 {
            for j in 0..10 {
                if i != j {
                    assert_eq!(
                        fitted.factor[(i, j)],
                        0.0,
                        "off-diagonal entry at ({i}, {j})"
                    );
                }
            }
            assert!(fitted.factor[(i, i)] > 0.0);
        }
    }

    #[test]
    fn a_constant_coordinate_still_fits_and_stays_reachable() {
        let mut rng = rng();
        let states: Vec<Vec<f64>> = (0..1_000)
            .map(|_| vec![gaussian(&mut rng), 3.0, gaussian(&mut rng)])
            .collect();
        let fitted = Preconditioner::fit(&states).expect("a floored diagonal factors");
        assert!(
            fitted.factor[(1, 1)] > 0.0,
            "the stuck coordinate was frozen out"
        );
        let bent = fitted.direction(&[0.0, 1.0, 0.0]);
        assert!(
            bent[1] > 0.0,
            "a push along the stuck coordinate should still move it"
        );
    }

    #[test]
    fn nothing_to_fit_is_no_transform() {
        assert!(Preconditioner::fit(&[]).is_none());
        assert!(
            Preconditioner::fit(&[vec![], vec![]]).is_none(),
            "no movable coordinates"
        );
        assert!(
            Preconditioner::fit(&[vec![1.0, 2.0]]).is_none(),
            "one state has no covariance"
        );
    }

    /// The direction distribution must stay symmetric for the uniform
    /// distribution to stay invariant, and a unit result keeps the chord
    /// arithmetic on the scale the sphere draw had.
    #[test]
    fn a_bent_direction_is_unit_and_odd() {
        let mut rng = rng();
        let states = gaussian_states(&mut rng, &[&[3.0, 0.0], &[1.0, 0.5]], 5_000);
        let fitted = Preconditioner::fit(&states).expect("fits");
        for _ in 0..100 {
            let unit = random_direction(&mut rng, 2);
            let bent = fitted.direction(&unit);
            let negated = fitted.direction(&[-unit[0], -unit[1]]);
            let norm = bent.iter().map(|c| c * c).sum::<f64>().sqrt();
            assert!((norm - 1.0).abs() < 1e-12, "{bent:?} is not a unit vector");
            assert_eq!(negated, vec![-bent[0], -bent[1]], "not odd in its argument");
        }
    }

    /// The point of the exercise: on a tube, directions follow the tube.
    #[test]
    fn a_tube_bends_directions_along_itself() {
        // Uniform in a tube of length ten and width a tenth, lying along the
        // diagonal — the shape no axis move can help with.
        let mut rng = rng();
        let axis = [
            std::f64::consts::FRAC_1_SQRT_2,
            std::f64::consts::FRAC_1_SQRT_2,
        ];
        let across = [-axis[1], axis[0]];
        let states: Vec<Vec<f64>> = (0..5_000)
            .map(|_| {
                let along: f64 = rng.random_range(-5.0..5.0);
                let sideways: f64 = rng.random_range(-0.05..0.05);
                vec![
                    along * axis[0] + sideways * across[0],
                    along * axis[1] + sideways * across[1],
                ]
            })
            .collect();
        let fitted = Preconditioner::fit(&states).expect("fits");

        let alignment = |direction: &[f64]| (direction[0] * axis[0] + direction[1] * axis[1]).abs();
        let draws = 2_000;
        let (mut sphere, mut bent) = (0.0, 0.0);
        for _ in 0..draws {
            let unit = random_direction(&mut rng, 2);
            sphere += alignment(&unit);
            bent += alignment(&fitted.direction(&unit));
        }
        let (sphere, bent) = (sphere / f64::from(draws), bent / f64::from(draws));
        // Uniform on the circle averages `2 / pi` of alignment with any axis.
        assert!(
            (sphere - 2.0 / std::f64::consts::PI).abs() < 0.05,
            "sphere: {sphere}"
        );
        assert!(
            bent > 0.95,
            "directions should hug the tube, got {bent} against the sphere's {sphere}"
        );
    }

    /// A chain over discrete coordinates alone moves from level to level and
    /// never between them: every move is along an axis, and every axis move
    /// lands on a level.
    #[test]
    fn a_discrete_chain_never_leaves_its_levels() {
        // setup
        let system = crate::system::tests::system(
            vec![
                crate::InputVariable::new_discrete("n", 0.0, 1.0, 20.0),
                crate::InputVariable::new_discrete("m", 0.0, 0.5, 10.0),
            ],
            &["n + m <= 20"],
        );
        let mut walker = super::HitAndRunWalker::new(rng());

        // act
        let points = walker.extend(&system, &[vec![0.0, 0.0]], 200);

        // assert
        assert_eq!(points.len(), 200);
        for point in &points {
            assert!(system.is_feasible(point, 0.0), "{point:?}");
        }
        let mut seen: Vec<(f64, f64)> = points.iter().map(|p| (p[0], p[1])).collect();
        seen.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        seen.dedup();
        assert!(
            seen.len() > 50,
            "the chain barely moved: {} points",
            seen.len()
        );
    }
}

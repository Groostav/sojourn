//! Rejection sampling over the input box: the brute squad.
//!
//! Cheap and unbiased, and it works whenever the feasible region is a decent
//! fraction of the box — or, given enough proposals, a small one. It cannot
//! find a measure-zero region: an equality constraint like
//! `x1 == sqrt(x2) +/- 0.0001` is a ribbon that uniform sampling will
//! essentially never land on. That is what the solver is for.
//!
//! Candidates are proposed as a matrix, one column each, filled in bulk from
//! a Xoshiro256++ stream: nine integer operations per draw where the ChaCha
//! stream it replaced spent a block cipher round, and no allocation per
//! candidate where there used to be one `Vec` each. The pool judges the whole
//! matrix with the batched evaluator, which is what made the fill the
//! bottleneck in the first place.
//!
//! When the pool's probe lands nothing, [`RandomSampler::search_for_seed`]
//! keeps proposing on every core until a batch lands or a proposal budget is
//! spent. Batches are numbered and each is a pure function of its number, so
//! the point that comes out does not depend on how many threads looked for it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use faer::Mat;
use rand::rngs::Xoshiro256PlusPlus;
use rand::{Rng, RngExt, SeedableRng};

use super::classify;
use super::progress::Trial;
#[cfg(feature = "gpu")]
use super::sieve::{GPU_BATCH, Sieve};
use crate::solve::GpuTarget;
use crate::{ConstraintSystem, InputVariable, Point};

/// How many candidates to propose per point asked for.
///
/// Proposals are cheap and the pool filters them, so over-proposing is how a
/// narrow-but-not-tiny region still yields points. Every candidate proposed is
/// now judged, where the one-at-a-time filter used to stop at `count` hits;
/// lowering this on a decided sampling route is a later knob, because it would
/// change which points come out and re-roll every seeded verdict.
const OVER_SAMPLING_FACTOR: usize = 100;

/// `2^-53`: the top 53 bits of a draw as a double in `[0, 1)`, which is what
/// rand's `StandardUniform` does too.
const UNIT: f64 = 1.0 / 9_007_199_254_740_992.0;

#[expect(
    clippy::cast_precision_loss,
    reason = "53 bits is exactly what an f64 mantissa holds"
)]
fn unit(bits: u64) -> f64 {
    (bits >> 11) as f64 * UNIT
}

/// Fills every column of `candidates` with one draw per row: row `r` takes
/// `low + u * (high - low)` for `(low, high) = bounds[r]`, `u` uniform in
/// `[0, 1)`. A row whose range is empty (`low >= high`) is filled with `low`.
///
/// **Half-open on purpose.** `random_range(low..=high)` reaches `high` with
/// probability `2^-53` per draw and pays a division for it; the difference is
/// measure zero, and the box check downstream is inclusive either way. A value
/// can land a last-place rounding past `high` only when `u` is within an ulp of
/// one, and that check rejects it — exactly as it rejected `random_range`'s own
/// overshoot.
///
/// Column-major: the outer loop is over columns because a column is contiguous
/// and a row is strided; the per-row `(low, scale)` table is computed once.
pub fn fill_box(candidates: &mut Mat<f64>, bounds: &[(f64, f64)], rng: &mut Xoshiro256PlusPlus) {
    assert_eq!(
        candidates.nrows(),
        bounds.len(),
        "one bound per row of the candidate matrix"
    );
    let table: Vec<(f64, f64)> = bounds
        .iter()
        .map(|&(low, high)| {
            if low >= high {
                (low, 0.0)
            } else {
                (low, high - low)
            }
        })
        .collect();

    for column in 0..candidates.ncols() {
        for (value, &(low, scale)) in candidates.col_as_slice_mut(column).iter_mut().zip(&table) {
            *value = low + unit(rng.next_u64()) * scale;
        }
    }
}

/// Bytes of candidate matrix per batch of the brute-force search. Sets the
/// batch width from the row count: under three thousand columns for three
/// variables, the floor for two hundred. Wide enough that the per-batch
/// bookkeeping is noise; narrow enough that two threads' batches share one
/// core's L2 when the thread count is the logical one.
///
/// Measured on the laptop (eight cores, sixteen threads, `x1 > 2` over three
/// variables): at 4 MiB sixteen threads managed 137M proposals a second and
/// were *slower* than eight; at 64 KiB they manage 450M. One thread is 75M
/// at any size, so this is a cache number and not an evaluator one.
const BATCH_BYTES: usize = 64 << 10;

/// Bounds on the batch width, whatever the row count says.
const MIN_BATCH_COLUMNS: usize = 256;
const MAX_BATCH_COLUMNS: usize = 16_384;

/// Uniform rejection sampling over the declared box, never narrowed.
///
/// Uniform over the feasible region by construction, which is what makes it
/// the probe that decides how a pool works, the thing that delivers where that
/// probe succeeds, and the reference the fairness oracles measure against.
/// Where the probe lands nothing it is also the brute squad: the same
/// proposals, wider, on every core, for a proposal budget, to land the seed
/// the walker needs.
///
/// An *adaptive* variant that narrowed the box toward points already found
/// existed and was removed on 2026-09-03: it was biased by construction, the
/// walker covers what it covered once a seed exists, and brute force is what
/// reaches a seed at all.
pub(crate) struct RandomSampler {
    rng: Xoshiro256PlusPlus,
    /// The declared box, as `(low, high)` per variable, in `fill_box`'s shape.
    bounds: Vec<(f64, f64)>,
    /// Candidates the brute-force search may propose before giving up. Zero
    /// disables it.
    budget: u64,
    /// Threads the brute-force search fans out over. Never changes what is
    /// found, only how soon.
    threads: usize,
    /// The GPU device configuration.
    #[cfg_attr(
        not(feature = "gpu"),
        allow(dead_code, reason = "knob exists without the feature")
    )]
    gpu: GpuTarget,
    /// The budget brute force may spend on a GPU.
    #[cfg_attr(
        not(feature = "gpu"),
        allow(dead_code, reason = "knob exists without the feature")
    )]
    gpu_proposals: u64,
}

impl RandomSampler {
    pub(crate) fn new(
        variables: &[InputVariable],
        rng: Xoshiro256PlusPlus,
        budget: u64,
        threads: usize,
        gpu: GpuTarget,
        gpu_proposals: u64,
    ) -> Self {
        let bounds = variables
            .iter()
            .map(|variable| (variable.lower_bound, variable.upper_bound))
            .collect();
        Self {
            rng,
            bounds,
            budget,
            threads: threads.max(1),
            gpu,
            gpu_proposals,
        }
    }

    /// One brute-force batch from the sampler's own stream, judged: the probe
    /// the pool decides its route on. Tens of microseconds for a few
    /// variables, and smaller than the delivery batches, which propose a
    /// hundred candidates per point asked for.
    pub(crate) fn probe(&mut self, problem: &ConstraintSystem) -> Trial {
        self.round(problem, self.batch_columns(), usize::MAX)
    }

    /// Up to `count` points from `count * OVER_SAMPLING_FACTOR` fresh
    /// candidates: the delivery batch on the sampling route.
    ///
    /// Every candidate is judged, where the one-at-a-time filter used to stop
    /// at `count` hits. The stream is consumed identically, so the points that
    /// come out are the same; only the judging of the surplus is extra, and a
    /// batch is what makes judging cheap.
    pub(crate) fn deliver(&mut self, problem: &ConstraintSystem, count: usize) -> Trial {
        if count == 0 {
            return Trial::default();
        }
        self.round(problem, count * OVER_SAMPLING_FACTOR, count)
    }

    /// One fill of `columns` candidates from the stream, judged, keeping at
    /// most `keep` of the hits.
    fn round(&mut self, problem: &ConstraintSystem, columns: usize, keep: usize) -> Trial {
        let mut candidates = Mat::zeros(self.bounds.len(), columns);
        fill_box(&mut candidates, &self.bounds, &mut self.rng);
        let mut points = problem.feasible_columns(candidates.as_ref());
        points.truncate(keep);
        Trial {
            points,
            proposed: columns as u64,
        }
    }

    /// Columns per batch of the brute-force search.
    fn batch_columns(&self) -> usize {
        let rows = self.bounds.len().max(1);
        (BATCH_BYTES / (rows * size_of::<f64>())).clamp(MIN_BATCH_COLUMNS, MAX_BATCH_COLUMNS)
    }

    /// Wide-batch rejection sampling on every core until a batch lands or the
    /// budget runs out.
    ///
    /// **The batch is the unit of randomness.** Batch `k` draws from a stream
    /// seeded by `(base, k)` and nothing else, and the result is the lowest
    /// numbered batch with a hit, so the points that come out are a function
    /// of the seed and the budget alone. Threads change how fast, never what.
    /// Threads that had run ahead finish the batch they are on, which is the
    /// only way `proposed` depends on the thread count. Every thread is
    /// joined before this returns: the fan-out is a detail of one call.
    ///
    /// Draws one value from the sampler's own stream for `base`. The pool
    /// reaches here only after an empty probe, after which it never asks this
    /// sampler for a batch again, so the probe and the delivery streams are
    /// exactly what they were before this existed.
    pub(crate) fn brute_force(&mut self, problem: &ConstraintSystem) -> Trial {
        let base = self.rng.next_u64();

        #[cfg(feature = "gpu")]
        if self.gpu != GpuTarget::Off
            && self.gpu_proposals > 0
            && let Some(sieve) = Sieve::new(problem, &self.gpu)
        {
            // The sieve — and with it the device, if nobody else holds it —
            // is dropped at the end of this block, whichever way it went.
            if let Some(trial) = brute_force_on_gpu(&sieve, problem, base, self.gpu_proposals) {
                return trial;
            }
            // The device failed or timed out mid-search. Finish on the CPU,
            // from the same base, and do not ask the device again.
            tracing::warn!("the GPU sieve stopped answering; finishing brute force on the CPU");
            self.gpu = GpuTarget::Off;
        }

        self.brute_force_on_cpu(problem, base)
    }

    /// The CPU brute-force loop: every thread walks its own arithmetic
    /// progression of batch numbers, and the lowest-numbered batch with a
    /// hit wins.
    fn brute_force_on_cpu(&self, problem: &ConstraintSystem, base: u64) -> Trial {
        let columns = self.batch_columns();
        let batches = self.budget.div_ceil(columns as u64);
        let rows = self.bounds.len();

        let winner = AtomicU64::new(u64::MAX);
        let best: Mutex<Option<(u64, Vec<Point>)>> = Mutex::new(None);
        let proposed = AtomicU64::new(0);

        // One thread's share of the batches: `first, first + stride, ...`,
        // stopping at the budget or at a batch past a known winner.
        let work = |first: u64, stride: u64| {
            let mut candidates = Mat::zeros(rows, columns);
            let mut k = first;
            while k < batches && k <= winner.load(Ordering::Acquire) {
                let mut rng = Xoshiro256PlusPlus::seed_from_u64(base ^ k);
                fill_box(&mut candidates, &self.bounds, &mut rng);
                // Driven coordinates are computed rather than guessed, which is
                // what lets a random draw land on a measure-zero surface at all.
                // Without it `y == sin(x) +/- 1e-9` is never hit and the budget
                // is spent in full -- not because no point exists, but because
                // nothing ever proposed one.
                //
                // **Here and not in `round`.** `round` is the probe and the
                // delivery path, and `Strategy::BruteSquad` is the fairness
                // oracle the benchmarks measure against; retracting there would
                // make the oracle a copy of the thing it judges. This is the
                // seed-finding path, which no oracle reads. The rng is seeded
                // per batch, so brute force stays a function of the seed and the
                // budget rather than of the thread count.
                // Every driven coordinate recomputed per column, under a plan
                // drawn per column — a branch chosen at random, which is what
                // a seed search wants; a problem without an equality pays
                // nothing here.
                if !problem.plans.is_empty() {
                    let rows = candidates.nrows();
                    for column in 0..candidates.ncols() {
                        let mut point: Point =
                            (0..rows).map(|row| candidates[(row, column)]).collect();
                        let plan = &problem.plans[rng.random_range(0..problem.plans.len())];
                        classify::retract(problem, plan, &mut point, &mut rng);
                        for (row, value) in point.into_iter().enumerate() {
                            candidates[(row, column)] = value;
                        }
                    }
                }
                let hits = problem.feasible_columns(candidates.as_ref());
                proposed.fetch_add(columns as u64, Ordering::Relaxed);

                if !hits.is_empty() {
                    winner.fetch_min(k, Ordering::AcqRel);
                    let mut best = best
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if best.as_ref().is_none_or(|(held, _)| k < *held) {
                        *best = Some((k, hits));
                    }
                }
                k += stride;
            }
        };

        let stride = self.threads as u64;
        std::thread::scope(|scope| {
            for first in 1..stride {
                let work = &work;
                scope.spawn(move || work(first, stride));
            }
            work(0, stride);
        });

        let points = best
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map(|(_, points)| points)
            .unwrap_or_default();
        Trial {
            points,
            proposed: proposed.into_inner(),
        }
    }
}

/// Brute force on the device: one dispatch per batch, sequentially, the
/// survivors of each re-judged exactly on the CPU. `None` when the device
/// stopped answering, with the caller expected to finish on the CPU.
///
/// Batch `k` is a function of `(base, k)` on a given device, and the batches
/// run in order, so the first batch with an exact hit is also the lowest —
/// the same contract as the CPU loop, without needing the winner bookkeeping.
#[cfg(feature = "gpu")]
fn brute_force_on_gpu(
    sieve: &Sieve,
    problem: &ConstraintSystem,
    base: u64,
    budget: u64,
) -> Option<Trial> {
    let mut proposed = 0u64;
    let batches = budget.div_ceil(u64::from(GPU_BATCH));
    for k in 0..batches {
        let remaining = budget - k * u64::from(GPU_BATCH);
        let count = u32::try_from(remaining.min(u64::from(GPU_BATCH))).expect("at most GPU_BATCH");
        let survivors = sieve.sieve_generated(base, k, count)?;
        proposed += u64::from(count);
        if survivors.is_empty() {
            continue;
        }
        let matrix = Mat::from_fn(problem.variables().len(), survivors.len(), |row, column| {
            survivors[column][row]
        });
        let points = problem.feasible_columns(matrix.as_ref());
        tracing::debug!(
            batch = k,
            survivors = survivors.len(),
            exact = points.len(),
            "GPU sieve batch"
        );
        if !points.is_empty() {
            return Some(Trial { points, proposed });
        }
    }
    Some(Trial {
        points: Vec::new(),
        proposed,
    })
}

#[cfg(test)]
mod tests {
    use faer::Mat;
    use rand::SeedableRng;
    use rand::rngs::Xoshiro256PlusPlus;

    use super::{UNIT, fill_box, unit};

    #[test]
    fn unit_spans_zero_to_just_below_one() {
        assert_eq!(unit(0), 0.0);
        assert_eq!(unit(1 << 11), UNIT);
        assert_eq!(unit(u64::MAX), 1.0 - UNIT);
        assert!(unit(u64::MAX) < 1.0);
    }

    #[test]
    fn fill_box_stays_inside_the_box_and_reaches_both_ends() {
        let bounds = [(-3.0, 7.0), (0.0, 1.0)];
        let mut candidates = Mat::zeros(2, 200_000);
        fill_box(
            &mut candidates,
            &bounds,
            &mut Xoshiro256PlusPlus::seed_from_u64(1),
        );

        for (row, &(low, high)) in bounds.iter().enumerate() {
            let values = (0..candidates.ncols()).map(|c| candidates[(row, c)]);
            let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
            for v in values {
                assert!(
                    v >= low && v < high,
                    "row {row}: {v} outside [{low}, {high})"
                );
                min = min.min(v);
                max = max.max(v);
            }
            let span = high - low;
            assert!(min < low + 1e-4 * span, "row {row}: never came near {low}");
            assert!(
                max > high - 1e-4 * span,
                "row {row}: never came near {high}"
            );
        }
    }

    #[test]
    fn fill_box_is_a_pure_function_of_the_seed() {
        let bounds = [(0.0, 10.0), (-1.0, 1.0), (5.0, 6.0)];
        let mut a = Mat::zeros(3, 1_000);
        let mut b = Mat::zeros(3, 1_000);
        fill_box(&mut a, &bounds, &mut Xoshiro256PlusPlus::seed_from_u64(42));
        fill_box(&mut b, &bounds, &mut Xoshiro256PlusPlus::seed_from_u64(42));
        for c in 0..1_000 {
            for r in 0..3 {
                assert_eq!(a[(r, c)].to_bits(), b[(r, c)].to_bits());
            }
        }
    }

    #[test]
    fn an_empty_range_fills_with_its_lower_bound() {
        let mut candidates = Mat::zeros(2, 50);
        fill_box(
            &mut candidates,
            &[(5.0, 5.0), (6.0, 5.0)],
            &mut Xoshiro256PlusPlus::seed_from_u64(3),
        );
        for c in 0..50 {
            assert_eq!(candidates[(0, c)], 5.0);
            assert_eq!(candidates[(1, c)], 6.0);
        }
    }
}

#[cfg(test)]
mod brute_force_tests {
    //! The budgeted search: what it finds is a function of the seed and the
    //! budget, and of nothing about the machine.

    use rand::SeedableRng;
    use rand::rngs::Xoshiro256PlusPlus;

    use super::{RandomSampler, Trial};
    use crate::system::tests::system;
    use crate::{GpuTarget, InputVariable};

    const SEED: u64 = 0xB2_07_E5_90_AD;

    /// Runs brute force on `x1 in 0..1` under `source`.
    fn search(source: &str, budget: u64, threads: usize) -> (Trial, usize) {
        let problem = system(vec![InputVariable::new("x1", 0.0, 1.0)], &[source]);
        let mut sampler = RandomSampler::new(
            &problem.variables,
            Xoshiro256PlusPlus::seed_from_u64(SEED),
            budget,
            threads,
            GpuTarget::Off,
            0,
        );
        let columns = sampler.batch_columns();
        let trial = sampler.brute_force(&problem);
        (trial, columns)
    }

    /// One in a million: the probe-sized first batch misses and the loop has
    /// to run tens of batches. Eight threads must land exactly the points one
    /// thread lands, because the winner is the lowest-numbered batch with a
    /// hit and a batch is a function of its number.
    #[test]
    fn the_same_seed_lands_the_same_points_on_one_thread_and_on_eight() {
        let (alone, columns) = search("x1 > 0.999999", 100_000_000, 1);
        let (crowd, _) = search("x1 > 0.999999", 100_000_000, 8);

        assert!(
            !alone.points.is_empty(),
            "one thread found nothing: {alone:?}"
        );
        for point in &alone.points {
            assert!(point[0] > 0.999_999, "{point:?}");
        }
        assert_eq!(alone.points, crowd.points);
        // Every batch below the winner is judged whoever runs it; the
        // overrun is whatever the other threads were doing at the time, which
        // depends on scheduling and is a handful of batches.
        assert!(crowd.proposed >= alone.proposed);
        assert!(
            crowd.proposed - alone.proposed <= 4 * 8 * columns as u64,
            "eight threads overran by {} batches",
            (crowd.proposed - alone.proposed) / columns as u64
        );
    }

    #[test]
    fn the_budget_bounds_the_proposals() {
        let (trial, columns) = search("x1 > 2", 100_000, 4);
        assert!(trial.points.is_empty());
        assert!(trial.proposed >= 100_000, "{}", trial.proposed);
        assert!(
            trial.proposed <= 100_000 + 4 * columns as u64,
            "{} proposals against a budget of 100,000",
            trial.proposed
        );
    }

    #[test]
    fn a_zero_budget_proposes_nothing() {
        let (trial, _) = search("x1 > 0.5", 0, 4);
        assert!(trial.points.is_empty());
        assert_eq!(trial.proposed, 0);
    }
}

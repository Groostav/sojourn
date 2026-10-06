//! Constrained random vector generation.
//!
//! Given a box of input variables and a set of babel constraints, produce
//! points that satisfy all of them and that cover the feasible region
//! reasonably evenly.
//!
//! Lives inside babel rather than alongside it so that [`crate::ast`] can stay
//! private — interval narrowing walks the AST, as an internal function over
//! it rather than a published consumer of it.
//!
//! # Strategy
//!
//! Finding the *first* feasible point is the hard part, and for a tight region
//! it needs a local solve or a bisection. Once there, cheap strategies cover
//! the space quickly. That is why `ConstraintSolver::solve` — the [`open`]
//! — is the expensive call, bounded by its budgets, and a [`design`] from the
//! region it found is not.
//!
//! This module is the engine. The types a caller holds — the system, the
//! solver, the region, the verdicts — are defined at the crate root
//! (`system.rs`, `solve.rs`, `repair.rs`) and this is what they drive.
//!
//! The strategies divide along that line:
//!
//! * **Uniform rejection sampling** — the brute squad — *probes*, and on a
//!   region it reaches often enough it simply delivers: unbiased by
//!   construction, no burn-in, no chain. Where the probe lands nothing and
//!   nothing else settled it, the same sampler keeps proposing on every
//!   core, for a proposal budget, until one batch lands: a region a millionth
//!   or a hundred-millionth of its box is a matter of milliseconds to seconds,
//!   and the seed it finds is what the walker starts from. The threads are
//!   joined before it returns; nothing runs after any call here.
//! * **Hit-and-run** *emits* everywhere the probe did not settle it. It
//!   converges to the uniform distribution over the region, so a design's
//!   candidate pool is drawn from a source with a guarantee. It cannot start
//!   without a feasible point, and a seed comes from the probe's own hits,
//!   from the local solve, from a bisection's leaves, or from brute force.
//! * **The local solve** ([`local`]) *seeds* where the probe found nothing:
//!   COBYLA from the box centre and a few seeded starts, stopped at the first
//!   point the oracle judges feasible. Finding one point of a nonlinear system
//!   is an ordinary constrained optimisation, and a local method does it in
//!   milliseconds at two hundred variables where brute force cannot find a
//!   region a millionth of its box. It cannot prove anything: a start that
//!   finds nothing says only that its basin held nothing.
//! * **Branch-and-prune** ([`prune`]) is the one that *proves*, and the one
//!   that finds the *pieces*. The declared box is contracted under every
//!   constraint before a single proposal — a plain contradiction empties a
//!   coordinate right there, and the constraints that emptied it are the
//!   blame in [`Infeasibility::Proved`]. When the walker will carry the
//!   search, or nothing has been found, the box is split and pruned for a
//!   budget of contractions, and the leaves that survive are where the
//!   region's pieces can be: a chain cannot cross between pieces, so each is
//!   seeded from its own leaf. What interval arithmetic cannot see — a
//!   contradiction every box encloses a little of — is left to brute force,
//!   and an empty search is [`Infeasibility::NotFound`], which claims
//!   nothing. Without it in the list nothing is proved and nothing is
//!   covered: the probe hands to the local solve and then to brute force.

pub(crate) mod classify;
#[cfg(feature = "gpu")]
#[doc(hidden)]
pub mod gpu;
pub(crate) mod hc4;
pub(crate) mod incidence;
pub(crate) mod interval;
pub(crate) mod local;
pub(crate) mod newton;
mod progress;
mod prune;
pub(crate) mod sampling;
#[cfg(feature = "gpu")]
mod sieve;
pub(crate) mod walking;

use std::collections::VecDeque;

use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;

use progress::{Progress, Trial};
use sampling::RandomSampler;
use walking::HitAndRunWalker;

use crate::solve::{Budgets, Strategy};
use crate::{ConstraintSystem, Point};

/// The hit rate below which the walker is expected to do the delivering.
///
/// A rate, judged on the probe. Below it the delivery batches — a hundred
/// candidates per point asked for — come back short often enough that the
/// walker fills most of every batch: at one in a thousand a batch for 32
/// points expects 3.2 hits; at one in ten thousand it is empty three times in
/// four. The JVM's `EASY_PATH_THRESHOLD_FACTOR` was a tenth of the points
/// *asked for* at hundredfold oversampling, which is the same rate.
///
/// This decides nothing about who runs — a design's pool is sampled first and
/// walked for the rest, see [`design`]. It decides only whether the opening
/// spends its prune budget on *coverage*: chains cannot cross between a
/// region's pieces, so a search the walker will carry needs a seed in every
/// piece, where uniform proposals reach every piece in proportion to its
/// measure and need no help. The probe is one batch and can misjudge the
/// rate either way; the cost of a wrong guess here is coverage of a rare
/// piece.
const EASY_PATH_THRESHOLD: f64 = 0.001;

/// The candidate pool a design is chosen from: this many per point asked
/// for, plus [`POOL_FLOOR`].
///
/// The pool is sampled and walked, and the walk is the cost: a point at two
/// hundred variables is some four hundred moves. Two per point keeps a
/// large design's walk at twice what the design itself would cost; the
/// floor is what gives a small design — the usual one, a handful of points
/// — enough candidates to be a choice at all, since the walker's output is
/// a chain, correlated step to step, and the selection needs a run of
/// neighbours to skip past. The selection is `O(count · pool · dimensions)`,
/// which at these sizes is nothing next to the walk.
const POOL_FACTOR: usize = 2;

/// See [`POOL_FACTOR`].
const POOL_FLOOR: usize = 128;

/// The strategies, holding nothing but their streams and their knobs.
///
/// Lives for one [`open`] and is dropped with it. What the caller holds
/// afterwards is [`FeasibleRegion`](crate::FeasibleRegion), which owns none of
/// this: only the points the opening found. What the search has *found* along
/// the way is not here either: that is a [`Progress`] value threaded through
/// the opening.
pub(crate) struct Ladder {
    /// Uniform rejection sampling over the declared box, if configured. The
    /// probe, and the brute squad where the opening finds nothing.
    sampler: Option<RandomSampler>,
    /// Whether the walker is configured: the opening spends its prune budget
    /// on coverage only when chains will be placed.
    walker: Option<HitAndRunWalker>,
    /// The stream a local solve draws its starts from, when
    /// [`Strategy::LocalSolve`] is configured. Runs on the opening, after the
    /// probe, and again inside any leaf a bisection isolates.
    local: Option<Xoshiro256PlusPlus>,
    /// The bisection budget, in contractions, when [`Strategy::Prune`] is
    /// configured. Not a strategy object: a contraction reads the whole
    /// system, and it runs on the opening rather than per batch.
    prune: Option<u32>,
    /// The stream the region's walker burns in under, whatever the strategy
    /// list says: a design is always walked. Drawn after the strategies'
    /// streams, so their draws are what they were.
    burn_in: Xoshiro256PlusPlus,
    /// The stream the region keeps for what it does after the opening: the
    /// reference point's extra starts and every repair's sampling box. Drawn
    /// last, for the same reason.
    repair_stream: Xoshiro256PlusPlus,
}

impl std::fmt::Debug for Ladder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ladder")
            .field("sampler", &self.sampler.is_some())
            .field("walker", &self.walker.is_some())
            .field("local", &self.local.is_some())
            .field("prune", &self.prune)
            .finish()
    }
}

impl Ladder {
    pub(crate) fn new(
        system: &ConstraintSystem,
        mut rng: Xoshiro256PlusPlus,
        strategies: &[Strategy],
        budgets: Budgets,
    ) -> Self {
        let mut ladder = Self {
            sampler: None,
            walker: None,
            local: None,
            prune: None,
            burn_in: Xoshiro256PlusPlus::seed_from_u64(0),
            repair_stream: Xoshiro256PlusPlus::seed_from_u64(0),
        };
        // Each strategy gets its own stream, derived from the one passed in and
        // drawn in list order, so that adding or removing a strategy does not
        // reseed the ones after it.
        for strategy in strategies {
            let stream = Xoshiro256PlusPlus::from_rng(&mut rng);
            match strategy {
                Strategy::Prune => ladder.prune = Some(budgets.prune),
                Strategy::BruteSquad => {
                    let sampler = RandomSampler::new(
                        &system.variables,
                        stream,
                        budgets.proposals,
                        budgets.threads,
                        budgets.gpu.clone(),
                        budgets.gpu_proposals,
                    );
                    ladder.sampler = Some(sampler);
                }
                Strategy::HitAndRun => ladder.walker = Some(HitAndRunWalker::new(stream)),
                Strategy::LocalSolve => ladder.local = Some(stream),
            }
        }
        ladder.burn_in = Xoshiro256PlusPlus::from_rng(&mut rng);
        ladder.repair_stream = Xoshiro256PlusPlus::from_rng(&mut rng);
        ladder
    }

    /// What the region keeps once the opening is done with the ladder: the
    /// walker it will burn in, and the stream its reference and its repairs
    /// draw from.
    pub(crate) fn into_kept(self) -> (HitAndRunWalker, Xoshiro256PlusPlus) {
        (HitAndRunWalker::new(self.burn_in), self.repair_stream)
    }

    /// Whether the walker will do most of the delivering, judged on the
    /// probe's hit rate against [`EASY_PATH_THRESHOLD`].
    ///
    /// What the opening asks before spending its prune budget on coverage.
    /// With no walker configured the answer is no whatever the rate: there
    /// are no chains to place, so nothing to cover for.
    fn walker_will_carry(&self, probe: &Trial) -> bool {
        if self.walker.is_none() {
            return false;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "sample counts are far below the f64 integer limit"
        )]
        let enough = probe.points.len() as f64 >= EASY_PATH_THRESHOLD * probe.proposed as f64;
        !enough
    }
}

/// What the opening concluded while looking for its first point.
///
/// Carries constraint *indices* rather than expressions; `solve` names them.
pub(crate) enum Opening {
    /// At least one sample is in hand. The invariant a returned `FeasibleRegion`
    /// rests on, and the reason a bisection that ran out of budget need not
    /// surface: a budget spent that still produced a point arrives here like
    /// any other success. `seeded` is the local solve's point from the box
    /// centre, when the opening ran one: the most interior point the search
    /// knows, and the reference a repair walks in from.
    Satisfied { seeded: Option<Point> },
    /// Interval reasoning proved the region empty.
    Impossible { blamed: Vec<usize> },
    /// Nothing found and nothing proven, carrying whatever interval reasoning
    /// could conclude nothing from — which is often why.
    Unproven { unexpressed: Vec<usize> },
}

/// How many leaves of a bisection a local solve is spent on, when nothing
/// is in hand.
///
/// The leaves a bisection ends with are candidates rather than pieces: at
/// the budget a region along a curve is a great many boxes, most of them
/// holding the same piece, and a local solve in each would be the budget
/// spent twice. A leaf that settled — its centre judged feasible — is a seed
/// for free and every one is taken; the ones that did not are solved
/// farthest-first, this many of them. Sixteen, the number of gap queries
/// this replaced, which found every piece any fixture has.
const GAP_SEEDS: usize = 16;

/// Seeds from the leaves of a bisection: the pieces of the region the search
/// has not reached.
///
/// **Hit-and-run cannot discover a component it was not started in.** A chain
/// samples the piece it began in and nothing else, so on `abs(x1) == 1 +/- 1e-9`
/// a search that starts from one root delivers that root five hundred times and
/// never learns the other exists. Somebody has to go looking *before* the chains
/// are placed, and a bisection is what looks: every leaf it ends with is a box
/// the constraints could not rule out, and a piece of the region is in one of
/// them.
///
/// A leaf holding a point already in hand is a piece already known and is
/// skipped. Of the rest, a settled leaf's centre is a seed as it stands, and
/// every one is taken. An unsettled leaf gets a local solve inside its own
/// box **only when nothing at all is in hand**: then the solve is the seed
/// search itself and worth paying for. With a point in hand it is not. A
/// bisection isolates a piece only where it can split enough coordinates —
/// a budget of four thousand contractions is twelve splits, which is a
/// piece at four dimensions and no piece at all at two hundred, where every
/// leaf is the whole box on the other hundred and eighty-eight coordinates
/// and a local solve in each is a failing start at two hundred variables,
/// minutes apiece. The pieces a bisection *can* isolate settle; the ones it
/// cannot, nothing here can find, and that is the limit recorded in
/// `docs/todo.md`. Everything returned has been judged where it was made.
fn cover(
    problem: &ConstraintSystem,
    leaves: Vec<prune::Leaf>,
    held: &VecDeque<Point>,
    local: Option<&mut Xoshiro256PlusPlus>,
) -> Vec<Point> {
    let mut seeds: Vec<Point> = Vec::new();
    let mut unsettled: Vec<prune::Node> = Vec::new();
    for leaf in leaves {
        if held.iter().any(|point| leaf.node.contains(point)) {
            continue;
        }
        match leaf.settled {
            Some(centre) => seeds.push(centre),
            None => unsettled.push(leaf.node),
        }
    }

    if held.is_empty()
        && let Some(rng) = local
    {
        // Farthest-first from what is held, in the box's own coordinates,
        // so a leaf on the far side of the region is solved before one
        // next to a known piece — the ordering `start_chains` uses for the
        // same reason.
        let widths = widths(problem);
        let centre = |node: &prune::Node| -> Point {
            node.bounds
                .iter()
                .map(|interval| interval.lo() + interval.width() / 2.0)
                .collect()
        };
        let mut remaining = GAP_SEEDS;
        while remaining > 0 && !unsettled.is_empty() {
            let known: Vec<&Point> = held.iter().chain(seeds.iter()).collect();
            let farthest = (0..unsettled.len())
                .map(|index| {
                    let at = centre(&unsettled[index]);
                    let nearest = known
                        .iter()
                        .map(|point| normalised_distance(&widths, &at, point))
                        .fold(f64::INFINITY, f64::min);
                    (index, nearest)
                })
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map_or(0, |(index, _)| index);
            let node = unsettled.swap_remove(farthest);
            remaining -= 1;
            if let Some(seed) = local::find_initial(problem, &node.bounds, 1, rng) {
                seeds.push(seed);
            }
        }
    }

    tracing::info!(seeds = seeds.len(), "coverage from a bisection's leaves");
    seeds
}

/// The opening: contract, probe, local solve, bisect, brute force, in that
/// order and with no flags between them. Each rung runs only if the ones
/// before it left nothing in hand — except the bisection, which also runs
/// for coverage when the walker will carry the search.
///
/// The contraction is microseconds and settles a plain contradiction before
/// a single proposal. The probe is one brute-force batch, tens of
/// microseconds, and settles most problems outright. The local solve finds a
/// point of a thin region in milliseconds. The bisection is what proves a
/// contradiction the contraction alone could not see, and what finds the
/// pieces of a region a chain cannot cross between; what it cannot decide
/// within its budget is exactly what brute force is for.
///
/// `Satisfied` means at least one feasible point is in hand, which is what
/// a returned [`FeasibleRegion`](crate::FeasibleRegion) promises.
pub(crate) fn open(
    problem: &ConstraintSystem,
    ladder: &mut Ladder,
    known: Vec<Point>,
) -> (Opening, Progress) {
    // Hints are judged, not trusted, and count as points rather than trials.
    let known: Vec<Point> = known
        .into_iter()
        .filter(|point| problem.is_feasible(point, 0.0))
        .collect();
    let progress = Progress::empty().extend(known);

    // What each constraint managed to say, over the whole opening: the
    // answer to "which constraints could nothing be concluded from".
    let mut contributed = vec![false; problem.constraints.len()];
    let root = match ladder.prune {
        None => None,
        Some(budget) => {
            match prune::contract(problem, prune::Node::declared(problem), &mut contributed) {
                prune::Contracted::Empty { blamed } => {
                    assert!(
                        progress.is_empty(),
                        "a point in hand contradicts a proof that there is none: {:?}",
                        progress.points()
                    );
                    return (Opening::Impossible { blamed }, progress);
                }
                prune::Contracted::Live(root) => Some((root, budget)),
            }
        }
    };

    let (mut progress, walker_will_carry) = match &mut ladder.sampler {
        Some(sampler) => {
            let probe = sampler.probe(problem);
            let carries = ladder.walker_will_carry(&probe);
            (progress.absorb(probe), carries)
        }
        None => (progress, ladder.walker.is_some()),
    };

    // A local solve before any bisection: finding one point is an
    // optimisation, and a local method does it where a bisection of two
    // hundred coordinates would spend its budget on the first few.
    let mut seeded = None;
    if progress.is_empty()
        && let Some(rng) = &mut ladder.local
        && let Some(point) = local::find_initial(problem, &problem.declared(), local::STARTS, rng)
    {
        progress = progress.extend(vec![point.clone()]);
        seeded = Some(point);
    }

    // Having points settles the *verdict*. It does not settle **coverage**:
    // hit-and-run cannot discover a component it was not started in, so a
    // search the walker will carry needs to know about the whole region
    // before its chains are placed — however the points it holds were come
    // by. A caller's hint and a local seed are every bit as single-component
    // as a witness, and `parabolic_roots_ribbon` hands in one point at
    // `x = -2` and never learns about the root at 1.
    //
    // A search sampling will carry is exempt, and that is not an oversight:
    // uniform proposals reach every component in proportion to its measure,
    // so discovery would be a budget spent for nothing.
    if let Some((root, budget)) = root
        && (progress.is_empty() || walker_will_carry)
    {
        match prune::bisect(problem, root, budget, &mut contributed) {
            prune::Pruned::Empty { blamed } => {
                assert!(
                    progress.is_empty(),
                    "a point in hand contradicts a proof that there is none: {:?}",
                    progress.points()
                );
                return (Opening::Impossible { blamed }, progress);
            }
            prune::Pruned::Live { leaves, exhausted } => {
                if exhausted {
                    tracing::warn!(
                        leaves = leaves.len(),
                        budget,
                        "bisection cut short; pieces beyond the seeds in hand may be missed"
                    );
                }
                let seeds = cover(problem, leaves, progress.points(), ladder.local.as_mut());
                progress = progress.extend(seeds);
            }
        }
    }

    if progress.is_empty()
        && let Some(sampler) = &mut ladder.sampler
    {
        let trial = sampler.brute_force(problem);
        tracing::info!(
            proposed = trial.proposed,
            landed = trial.points.len(),
            "brute force"
        );
        progress = progress.absorb(trial);
    }

    if !progress.is_empty() {
        tracing::debug!(
            points = progress.points().len(),
            proposed = progress.proposed(),
            landed = progress.landed(),
            "opened"
        );
        return (Opening::Satisfied { seeded }, progress);
    }
    // Without a contractor every constraint is "unexpressed" in the sense
    // `NotFound` uses: none was put to anything that could reason about it.
    let unexpressed = contributed
        .iter()
        .enumerate()
        .filter_map(|(index, said)| (!said).then_some(index))
        .collect();
    (Opening::Unproven { unexpressed }, progress)
}

/// A space-filling design of up to `count` feasible points, spread away from
/// `existing` and from each other.
///
/// The pool: `held` (what the opening found, the witness first), a round of
/// uniform proposals over the box, and the walker for the rest — sampling
/// first because it is unbiased and on a region it reaches it is the whole
/// pool; the walker is what reaches a region sampling cannot. The walker is
/// the region's, its chains burnt in at `solve`, cloned and reseeded here;
/// the proposals and the walk draw from streams derived from `rng`, so the
/// pool is a function of the region and the stream. Then farthest-first: each
/// choice is the pool point whose nearest neighbour among `existing` and the
/// choices so far is farthest, in box-normalised Euclidean distance, the
/// metric `repair` lands by. With nothing to spread from, the first choice is
/// the witness. A Latin hypercube was considered and is the wrong tool here:
/// it stratifies each axis, which says nothing about a design of fewer
/// points than dimensions, where "far from each other and from what the
/// caller has" is the whole requirement.
///
/// Fewer than `count` only when the region ran out of distinct points: the
/// next choice would coincide with one already in the design, which a
/// point-sized region reaches at once (the walker stays put, and emits the
/// same point). Every point returned is feasible and judged here.
pub(crate) fn design(
    problem: &ConstraintSystem,
    walker: &HitAndRunWalker,
    held: &[Point],
    existing: &[Point],
    count: usize,
    mut rng: Xoshiro256PlusPlus,
) -> Vec<Point> {
    let _span = tracing::debug_span!("design", count, existing = existing.len()).entered();
    if count == 0 || held.is_empty() {
        return Vec::new();
    }
    let mut sampler = RandomSampler::new(
        &problem.variables,
        Xoshiro256PlusPlus::from_rng(&mut rng),
        0,
        1,
        crate::solve::GpuTarget::Off,
        0,
    );
    let mut walker = walker
        .clone()
        .reseeded(Xoshiro256PlusPlus::from_rng(&mut rng));

    let wanted = count * POOL_FACTOR + POOL_FLOOR;
    let mut pool: Vec<Point> = held.to_vec();
    let trial = sampler.deliver(problem, wanted);
    let sampled = trial.points.len();
    pool.extend(trial.points);
    let walked: Vec<Point> = walker
        .extend(problem, &pool, wanted - sampled)
        .into_iter()
        .filter(|point| problem.is_feasible(point, 0.0))
        .collect();
    tracing::debug!(sampled, walked = walked.len(), "pooled");
    pool.extend(walked);

    let widths = widths(problem);
    let mut nearest: Vec<f64> = pool
        .iter()
        .map(|candidate| {
            existing
                .iter()
                .map(|point| normalised_distance(&widths, candidate, point))
                .fold(f64::INFINITY, f64::min)
        })
        .collect();
    let mut chosen: Vec<Point> = Vec::with_capacity(count);
    while chosen.len() < count {
        // `max_by` keeps the last of equal maxima; with nothing to spread
        // from every candidate is infinitely far, and the one to take is the
        // witness at the front.
        let (index, farthest) = nearest
            .iter()
            .copied()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map_or((0, 0.0), |(index, farthest)| {
                if farthest.is_infinite() {
                    (0, farthest)
                } else {
                    (index, farthest)
                }
            });
        if farthest <= 0.0 {
            break;
        }
        let choice = pool[index].clone();
        for (candidate, nearest) in pool.iter().zip(&mut nearest) {
            *nearest = nearest.min(normalised_distance(&widths, candidate, &choice));
        }
        chosen.push(choice);
    }
    tracing::debug!(chosen = chosen.len(), pool = pool.len(), "designed");
    chosen
}

/// Each variable's box width, the scale [`normalised_distance`] divides by.
pub(crate) fn widths(problem: &ConstraintSystem) -> Vec<f64> {
    problem
        .variables
        .iter()
        .map(|variable| variable.upper_bound - variable.lower_bound)
        .collect()
}

/// Euclidean distance over box-normalised coordinates: the metric a landing
/// is judged nearest by and a design is spread by. A coordinate with no width
/// contributes nothing.
pub(crate) fn normalised_distance(widths: &[f64], a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .zip(widths)
        .map(|((x, y), width)| {
            if *width > 0.0 {
                let scaled = (x - y) / width;
                scaled * scaled
            } else {
                0.0
            }
        })
        .sum::<f64>()
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConstraintSolver, Infeasibility, InputVariable};

    /// A region one millionth of its box: the probe misses, brute force
    /// lands a seed, the walker delivers from it.
    fn one_in_a_million() -> ConstraintSystem {
        ConstraintSystem::new(vec![InputVariable::new("x1", 0.0, 1.0)], ["x1 > 0.999999"])
            .expect("the fixture binds")
    }

    const SEED: u64 = 0x50_50_1E_5E_ED;

    #[test]
    fn brute_force_seeds_the_walker_when_the_probe_is_empty() {
        let verdict = ConstraintSolver::new()
            .with_strategies(vec![Strategy::BruteSquad, Strategy::HitAndRun])
            .with_threads(2)
            .solve(
                &one_in_a_million(),
                &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
            );

        let region = verdict.expect("brute force should have found the region");
        let delivered = region
            .sample(
                faer::Mat::zeros(1, 0).as_ref(),
                10,
                &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
            )
            .expect("a slab has ten distinct points");
        assert_eq!(delivered.ncols(), 10);
        for column in 0..10 {
            assert!(
                delivered[(0, column)] > 0.999_999,
                "{}",
                delivered[(0, column)]
            );
        }
    }

    /// The order of escalation: contract, probe, local solve, bisect, brute
    /// force. With no proposal budget at all a region the seeders can reach
    /// is still found, because they go first; a region interval reasoning
    /// cannot settle within its budget — a computed subscript, which it
    /// concludes nothing from — is found anyway, because what it cannot
    /// decide is handed to brute force.
    #[test]
    fn the_seeders_go_first_and_brute_force_takes_what_they_cannot_decide() {
        let by_seeders = ConstraintSolver::new().with_proposal_budget(0).solve(
            &one_in_a_million(),
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        );
        assert!(
            by_seeders.is_ok(),
            "the contraction or the local solve should have seeded the region with no brute \
             force at all: {by_seeders:?}"
        );

        // `var[n]` with `n` pinned to 1 is `x1 > 0.99999` written so that
        // nothing but the evaluator can read it: one in a hundred thousand,
        // ten expected hits in the budget below.
        let opaque = ConstraintSystem::new(
            vec![
                InputVariable::new("x1", 0.0, 1.0),
                InputVariable::new("n", 1.0, 1.0),
            ],
            ["var[floor(n)] > 0.99999"],
        )
        .expect("the fixture binds");
        let by_brute_force = ConstraintSolver::new()
            .with_strategies(vec![
                Strategy::BruteSquad,
                Strategy::HitAndRun,
                Strategy::Prune,
            ])
            .with_proposal_budget(1_000_000)
            .with_threads(2)
            .solve(&opaque, &mut Xoshiro256PlusPlus::seed_from_u64(SEED));
        let region = by_brute_force
            .expect("brute force should have taken over from what nothing could conclude on");
        let delivered = region
            .sample(
                faer::Mat::zeros(2, 0).as_ref(),
                5,
                &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
            )
            .expect("a slab has five distinct points");
        assert_eq!(delivered.ncols(), 5);
        for column in 0..5 {
            assert!(
                delivered[(0, column)] > 0.99999,
                "{}",
                delivered[(0, column)]
            );
        }
    }

    /// The prune budget reaches the bisection through the builder: an
    /// instance it would grind on comes back `NotFound` promptly, with no
    /// brute force to mask it — a small budget, and no more contractions
    /// than it allows.
    #[test]
    fn the_prune_budget_bounds_the_opening() {
        let hard = ConstraintSystem::new(
            vec![
                InputVariable::new("x", 0.0, 100.0),
                InputVariable::new("y", 0.0, 100.0),
                InputVariable::new("z", 0.0, 100.0),
            ],
            [
                "floor(x) * floor(y) == floor(z) * 7 + 3 +/- 0.000000001",
                "x*y*z == 12345.678 +/- 0.000000001",
                "x^2 + y^2 == z^2 + 1 +/- 0.000000001",
            ],
        )
        .expect("the fixture binds");

        let started = std::time::Instant::now();
        let verdict = ConstraintSolver::new()
            .with_prune_budget(64)
            .with_proposal_budget(0)
            .with_gpu_proposal_budget(0)
            .solve(&hard, &mut Xoshiro256PlusPlus::seed_from_u64(SEED));
        let took = started.elapsed();
        assert!(
            matches!(verdict, Err(Infeasibility::NotFound { .. })),
            "{verdict:?}"
        );
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    }

    /// Pins that the loop is what changed: with no proposal budget and no
    /// seeder the pool behaves as it did before step 4 and gives up after
    /// the probe.
    #[test]
    fn a_zero_budget_is_the_old_behaviour() {
        let verdict = ConstraintSolver::new()
            .with_strategies(vec![Strategy::BruteSquad, Strategy::HitAndRun])
            .with_proposal_budget(0)
            .with_gpu_proposal_budget(0)
            .solve(
                &one_in_a_million(),
                &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
            );

        assert!(
            matches!(verdict, Err(Infeasibility::NotFound { .. })),
            "{verdict:?}"
        );
    }
}

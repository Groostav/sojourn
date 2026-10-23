//! Interval contraction and bisection: the proof that a region is empty, the
//! constraints to blame for it, and the pieces of a region the walker must be
//! started in.
//!
//! # One mechanism, two jobs
//!
//! [`IntervalTape::narrow`] is HC4-revise: given every other symbol's interval,
//! the interval one symbol may take if a constraint is to hold, sound as a
//! superset. The walker asks it from a *point*, one coordinate at a time. Ask
//! it over the declared *box* instead — every constraint, every symbol it
//! names, intersected into the box and repeated until nothing narrows — and
//! that is HC4 propagation, the classical contractor. A coordinate that goes
//! empty is a **proof** that no point satisfies the constraints together,
//! and the constraints that narrowed it are the ones to **blame**.
//!
//! A box that survives is split down its widest coordinate and each half is
//! contracted again. Halves that die are pruned; halves that live are split
//! again, until they are settled, too small to split, or the budget is spent.
//! That is branch-and-prune, and what it leaves is a set of **leaves**: boxes
//! the region may still have a piece in, which is what
//! hit-and-run — unable to cross between pieces — needs to be started in. So
//! the same run that proves a contradiction covers a region in pieces.
//!
//! # What it cannot see
//!
//! Interval arithmetic encloses; it does not decide. A contradiction it
//! misses is one every box encloses a little of: a *thin* one, `x + y <= 1`
//! against `x + y >= 1 + 1e-9`, that bisection reaches only at a width of
//! `1e-9`; or an *algebraic* one, `x*x - 2*x*y + y*y < 0`, whose enclosure
//! holds negatives on every box because the square is never seen as a square.
//! Those end `Live` with the budget spent, and the search reports
//! [`Infeasibility::NotFound`](crate::Infeasibility::NotFound) rather than
//! claiming what it cannot show. A decision procedure would prove them; the
//! one this crate used to link was Z3, and the record of why it left is in
//! `docs/todo.md`.
//!
//! # Blame is the trace, not an analysis
//!
//! Each box carries, per coordinate, the constraints that narrowed it. When a
//! coordinate empties, the blame is the constraint that emptied it, the
//! constraints that narrowed it before, the constraints that narrowed the
//! coordinates *those* read, and so on backwards to a fixpoint — nothing more,
//! and nothing re-run to check. A constraint that narrowed nothing is never
//! blamed. Over a whole run the same trace says which constraints never
//! narrowed anything at all, which is what a `NotFound` names: the ones
//! interval reasoning could conclude nothing from.
//!
//! # Counts, not clocks
//!
//! A contraction ends when no coordinate shrinks by more than [`PROGRESS`] of
//! its declared width or after [`VISITS_PER_CONSTRAINT`] visits per
//! constraint, whichever first; a bisection ends at its budget of
//! contractions. Nothing here waits on anything, so nothing here can hang.
//!
//! [`IntervalTape::narrow`]: super::hc4::IntervalTape::narrow

use std::collections::VecDeque;

use super::classify;
use super::hc4::Frames;
use super::incidence::{ConstraintId, Row};
use super::interval::Interval;
use super::support::VariableSupport;
use crate::ast::GlobalId;
use crate::{ConstraintSystem, Point};

/// The fraction of a coordinate's declared width it must shrink by for the
/// shrink to count: to be applied, to be recorded against the constraint that
/// made it, and to send the constraints naming the coordinate round again.
///
/// Anything less is treated as no change. That is what makes a contraction
/// terminate on a chain that climbs a hair a sweep — `x >= y + 1`,
/// `y >= z + 1`, `z >= x + 1` on a wide box — and it is sound, since not
/// narrowing is never wrong. One per cent, because a step that small is a
/// step bisection will take anyway, and far more cheaply than a crawl.
const PROGRESS: f64 = 0.01;

/// How many times a contraction may visit each constraint, on average,
/// before it stops where it is.
///
/// The threshold above is what ends a contraction in practice; this is the
/// ceiling that makes "in practice" a guarantee. A contraction that has
/// visited every constraint this many times is one that is still finding one
/// per cent somewhere, which a wide, deep chain can do a while, and it is
/// stopped rather than left to finish: what it has is sound and bisection
/// resumes from it.
const VISITS_PER_CONSTRAINT: usize = 32;

/// Below this fraction of its declared width a coordinate is not split any
/// further, and a box with no coordinate wider is a leaf as it stands.
///
/// A leaf this small that is not settled is one the constraints hold nothing
/// visibly in and something invisibly — the thin and algebraic cases of the
/// module doc — and splitting it further would spend the budget seeing the
/// same thing at every scale.
const RESOLUTION: f64 = 1e-6;

/// A box: one interval per coordinate, in row order.
pub(crate) type Bounds = Vec<Interval>;

/// A box with its history: which constraints narrowed each of its
/// coordinates, from the declared box down to this one.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) bounds: Bounds,
    /// Per row, the constraints that narrowed it, in the order they did.
    narrowed_by: Vec<Vec<ConstraintId>>,
}

impl Node {
    /// The declared box, narrowed by nothing yet.
    pub(crate) fn declared(problem: &ConstraintSystem) -> Self {
        Self {
            bounds: problem.declared(),
            narrowed_by: vec![Vec::new(); problem.variables.len()],
        }
    }

    /// The point in the middle of every coordinate — for a discrete one, the
    /// level nearest the middle among those the box holds, since a seed off
    /// its levels is no seed. A box holding none of its levels keeps the
    /// middle; it is judged infeasible like any other miss.
    fn centre(&self, problem: &ConstraintSystem) -> Point {
        self.bounds
            .iter()
            .zip(&problem.supports)
            .map(|(interval, support)| {
                let middle = interval.lo() + interval.width() / 2.0;
                match support {
                    VariableSupport::Continuous { .. } => middle,
                    VariableSupport::Discrete(levels) => {
                        levels.within(*interval).map_or(middle, |held| {
                            let nearest = levels.nearest(middle).clamp(*held.start(), *held.end());
                            levels.level(nearest)
                        })
                    }
                }
            })
            .collect()
    }

    /// Whether `point` lies in the box, closed on every side.
    pub(crate) fn contains(&self, point: &[f64]) -> bool {
        self.bounds
            .iter()
            .zip(point)
            .all(|(interval, value)| interval.contains(*value))
    }

    /// Everything to blame for `culprit` emptying `row` here: the culprit,
    /// what narrowed the row before it, what narrowed the rows *those* read,
    /// and so on back to the declared box. Sorted, so the answer is a set.
    fn blame(&self, problem: &ConstraintSystem, culprit: ConstraintId, row: Row) -> Vec<usize> {
        let mut blamed = vec![false; problem.constraints.len()];
        let mut pending: Vec<ConstraintId> = Vec::new();
        let mut accuse = |id: ConstraintId, pending: &mut Vec<ConstraintId>| {
            if !blamed[id.index()] {
                blamed[id.index()] = true;
                pending.push(id);
            }
        };

        accuse(culprit, &mut pending);
        for id in &self.narrowed_by[row.index()] {
            accuse(*id, &mut pending);
        }
        while let Some(id) = pending.pop() {
            for read in problem.incidence.rows_of(id) {
                for narrower in &self.narrowed_by[read.index()] {
                    accuse(*narrower, &mut pending);
                }
            }
        }

        blamed
            .iter()
            .enumerate()
            .filter_map(|(index, accused)| accused.then_some(index))
            .collect()
    }
}

/// What a contraction concluded.
#[derive(Debug)]
pub(crate) enum Contracted {
    /// No point of the box satisfies the constraints together; `blamed` are
    /// the constraint indices that show it.
    Empty { blamed: Vec<usize> },
    /// The box the constraints could not rule out, no larger than the one
    /// given and possibly much smaller.
    Live(Node),
}

/// Contracts `node` to a fixpoint under every constraint of `problem`.
///
/// `contributed[c]` is set whenever constraint `c` narrows anything, and is
/// never cleared: it accumulates over a whole run, which is what makes it
/// the answer to "which constraints said nothing".
pub(crate) fn contract(
    problem: &ConstraintSystem,
    mut node: Node,
    contributed: &mut [bool],
) -> Contracted {
    let constraints = problem.constraints.len();
    let declared = super::widths(problem);

    // A worklist with an "already waiting" flag, so a constraint is queued at
    // most once however many of its rows moved since it was last seen.
    let mut queue: VecDeque<ConstraintId> = (0..constraints).map(ConstraintId).collect();
    let mut queued = vec![true; constraints];
    let mut visits = 0;
    let cap = VISITS_PER_CONSTRAINT * constraints;
    let mut frames = Frames::default();

    while let Some(id) = queue.pop_front() {
        queued[id.index()] = false;
        visits += 1;
        if visits > cap {
            tracing::debug!(visits, "contraction stopped at its visit cap");
            break;
        }

        let rows = problem.incidence.rows_of(id);
        let mut globals: Vec<Interval> = rows.iter().map(|row| node.bounds[row.index()]).collect();
        for (symbol, row) in rows.iter().enumerate() {
            let wanted = u32::try_from(symbol).expect("fewer than four billion symbols");
            let before = globals[symbol];
            // Onto the coordinate's levels, if it has any: an interval between
            // two levels is empty here, which is the proof the interval alone
            // could not give.
            let support = &problem.supports[row.index()];
            let after = support.round_inward(before.intersect(
                problem.constraints[id.index()].intervals.narrow(
                    &globals,
                    GlobalId::from_index(wanted),
                    &mut frames,
                ),
            ));
            if after.is_empty() {
                contributed[id.index()] = true;
                return Contracted::Empty {
                    blamed: node.blame(problem, id, *row),
                };
            }
            // Less than `PROGRESS` of the declared width is no change at all:
            // not applied, not recorded, not propagated. A discrete coordinate
            // has moved when it holds a level fewer, however small a share of
            // its width that is — a crawl a level a visit on a long lattice is
            // what the visit cap is for.
            let progressed = match support {
                VariableSupport::Continuous { .. } => {
                    before.width() - after.width() > PROGRESS * declared[row.index()]
                }
                VariableSupport::Discrete(levels) => {
                    levels.count_within(after) < levels.count_within(before)
                }
            };
            if !progressed {
                continue;
            }
            globals[symbol] = after;
            node.bounds[row.index()] = after;
            node.narrowed_by[row.index()].push(id);
            contributed[id.index()] = true;
            for naming in problem.incidence.naming(*row) {
                if !queued[naming.index()] {
                    queued[naming.index()] = true;
                    queue.push_back(*naming);
                }
            }
        }
    }

    Contracted::Live(node)
}

/// A box branch-and-prune could not rule out.
#[derive(Debug)]
pub(crate) struct Leaf {
    pub(crate) node: Node,
    /// The box's centre, when it was judged feasible — a piece of the region
    /// in hand, and the reason the box was not split further.
    pub(crate) settled: Option<Point>,
}

/// What a bisection concluded.
#[derive(Debug)]
pub(crate) enum Pruned {
    /// Every box died: no point of the root satisfies the constraints
    /// together, and `blamed` is the union of what each death showed.
    Empty { blamed: Vec<usize> },
    /// The boxes still standing when the run ended.
    Live {
        leaves: Vec<Leaf>,
        /// Whether the run ended on its budget or a cancellation rather than
        /// by running out of boxes to split — in which case the leaves are
        /// where it stopped rather than where the region is.
        exhausted: bool,
    },
}

/// Branch-and-prune from `root`, which has been contracted already, for at
/// most `budget` further contractions.
///
/// Breadth-first, so the budget is spread across the pieces of the region
/// rather than spent going deep into the first. A box whose centre — driven
/// coordinates centred in their bands, see [`classify::centre`] — is judged feasible is
/// settled and not split: it is a piece in hand. A box with no coordinate
/// wider than [`RESOLUTION`] is a leaf as it stands.
///
/// A discrete coordinate splits between two of its levels, so neither half
/// holds the other's, and is wide while it holds two levels or more, by its
/// share of its levels rather than of its width. A box narrowed to a single
/// value in every coordinate — one level, or an interval of no width — is a
/// point, and judging it decides it: feasible, it is settled; not, the box
/// is empty, blamed on what the point violates and on what narrowed the box
/// to it. That is what makes enumeration a proof — a small lattice split
/// down to its points, every one judged — and it holds for a continuous
/// coordinate of no width as well, which is a point by the same argument.
pub(crate) fn bisect(
    problem: &ConstraintSystem,
    root: Node,
    budget: u32,
    contributed: &mut [bool],
) -> Pruned {
    let declared = super::widths(problem);
    // The coordinate to split a box on: the widest relative to its declared
    // width, if that is wide enough to be worth splitting.
    let widest = |node: &Node| -> Option<usize> {
        node.bounds
            .iter()
            .zip(&declared)
            .zip(&problem.supports)
            .enumerate()
            .filter_map(|(row, ((interval, width), support))| match support {
                VariableSupport::Continuous { .. } => {
                    let relative = interval.width() / width;
                    (*width > 0.0 && relative >= RESOLUTION).then_some((row, relative))
                }
                VariableSupport::Discrete(levels) => {
                    let held = levels.count_within(*interval);
                    // Two levels held means at least two declared, so the
                    // division is by one or more.
                    (held >= 2).then(|| (row, (held - 1) as f64 / (levels.count() - 1) as f64))
                }
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(row, _)| row)
    };
    // Whether every coordinate of `node` holds one value, so its centre is
    // the whole box.
    let point = |node: &Node| -> bool {
        node.bounds
            .iter()
            .zip(&problem.supports)
            .all(|(interval, support)| match support {
                VariableSupport::Continuous { .. } => interval.lo() == interval.hi(),
                VariableSupport::Discrete(levels) => levels.count_within(*interval) == 1,
            })
    };
    let settle = |node: Node| -> Leaf {
        // The centre, with every driven coordinate put at the centre of its
        // band: the one move that puts a point of a thick box onto a thin
        // surface, and a no-op where nothing is driven.
        let mut centre = node.centre(problem);
        classify::centre(problem, &mut centre);
        let settled = problem.is_feasible(&centre, 0.0).then_some(centre);
        Leaf { node, settled }
    };

    let mut queue: VecDeque<Node> = VecDeque::from([root]);
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut blamed: Vec<bool> = vec![false; problem.constraints.len()];
    let mut remaining = budget;
    let mut exhausted = false;

    while let Some(node) = queue.pop_front() {
        let leaf = settle(node);
        if leaf.settled.is_some() {
            leaves.push(leaf);
            continue;
        }
        let Some(row) = widest(&leaf.node) else {
            if !point(&leaf.node) {
                leaves.push(leaf);
                continue;
            }
            // Judged as it stands: `classify::centre` moves a driven
            // coordinate within its slice of the declared box, which could
            // leave a box that is a single point.
            let only = leaf.node.centre(problem);
            if problem.is_feasible(&only, 0.0) {
                leaves.push(Leaf {
                    node: leaf.node,
                    settled: Some(only),
                });
                continue;
            }
            let mut violated = 0;
            for (index, constraint) in problem.constraints.iter().enumerate() {
                let id = ConstraintId(index);
                let rows = problem.incidence.rows_of(id);
                if constraint.holds(&only, rows, &problem.supports, 0.0, &mut []) {
                    continue;
                }
                violated += 1;
                for row in rows {
                    for index in leaf.node.blame(problem, id, *row) {
                        blamed[index] = true;
                    }
                }
            }
            assert!(
                violated > 0,
                "a point inside its box that fails the oracle fails a constraint: {only:?}"
            );
            continue;
        };
        if remaining < 2 {
            exhausted = true;
            leaves.push(leaf);
            continue;
        }
        remaining -= 2;

        let whole = leaf.node.bounds[row];
        let halves = match &problem.supports[row] {
            VariableSupport::Continuous { .. } => {
                let middle = whole.lo() + whole.width() / 2.0;
                (
                    Interval::new(whole.lo(), middle),
                    Interval::new(middle, whole.hi()),
                )
            }
            VariableSupport::Discrete(levels) => levels
                .split(whole)
                .expect("`widest` chose it for holding two levels"),
        };
        for half in [halves.0, halves.1] {
            let mut child = leaf.node.clone();
            child.bounds[row] = half;
            match contract(problem, child, contributed) {
                Contracted::Empty { blamed: shown } => {
                    for index in shown {
                        blamed[index] = true;
                    }
                }
                Contracted::Live(child) => queue.push_back(child),
            }
        }
    }

    tracing::info!(
        leaves = leaves.len(),
        settled = leaves.iter().filter(|leaf| leaf.settled.is_some()).count(),
        contractions = budget - remaining,
        budget,
        exhausted,
        silent = contributed.iter().filter(|said| !**said).count(),
        "branch-and-prune"
    );

    if leaves.is_empty() {
        debug_assert!(
            !exhausted,
            "a run that was cut short holds what it was cut short on"
        );
        Pruned::Empty {
            blamed: blamed
                .iter()
                .enumerate()
                .filter_map(|(index, accused)| accused.then_some(index))
                .collect(),
        }
    } else {
        Pruned::Live { leaves, exhausted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InputVariable;
    use crate::system::tests::system;

    fn contracted(system: &ConstraintSystem) -> (Contracted, Vec<bool>) {
        let mut contributed = vec![false; system.constraints.len()];
        let result = contract(system, Node::declared(system), &mut contributed);
        (result, contributed)
    }

    fn pruned(system: &ConstraintSystem, budget: u32) -> Pruned {
        pruned_with_silence(system, budget).0
    }

    /// [`pruned`], with the constraints that narrowed nothing.
    fn pruned_with_silence(system: &ConstraintSystem, budget: u32) -> (Pruned, Vec<usize>) {
        let mut contributed = vec![false; system.constraints.len()];
        let result = match contract(system, Node::declared(system), &mut contributed) {
            Contracted::Empty { blamed } => Pruned::Empty { blamed },
            Contracted::Live(root) => bisect(system, root, budget, &mut contributed),
        };
        let silent = contributed
            .iter()
            .enumerate()
            .filter_map(|(index, said)| (!said).then_some(index))
            .collect();
        (result, silent)
    }

    /// The plainest contradiction, and both sides of it are to blame: either
    /// alone is satisfiable, so naming one would be picking arbitrarily.
    #[test]
    fn a_plain_contradiction_is_empty_and_blames_both_sides() {
        let system = system(
            vec![InputVariable::new_continuous("x", 0.0, 10.0)],
            &["x > 8", "x < 2"],
        );
        let (result, _) = contracted(&system);
        let Contracted::Empty { blamed } = result else {
            panic!("{result:?}");
        };
        assert_eq!(blamed, vec![0, 1]);
    }

    /// The mistake a user makes: three constraints that describe a region and
    /// a fourth with its comparison backwards. The blame is the fourth and
    /// the two it contradicts; the one that narrowed nothing is not named.
    #[test]
    fn a_backwards_comparison_is_blamed_with_what_it_contradicts() {
        let system = system(
            vec![
                InputVariable::new_continuous("x1", 0.0, 10.0),
                InputVariable::new_continuous("x2", 0.0, 10.0),
            ],
            &["x1 < 3", "x2 > 5", "x1 + x2 < 20", "x1 > x2"],
        );
        let (result, _) = contracted(&system);
        let Contracted::Empty { blamed } = result else {
            panic!("{result:?}");
        };
        assert_eq!(blamed, vec![0, 1, 3]);
    }

    /// Soundness: a point known feasible is still inside the contracted box,
    /// on every shape the corpus has.
    #[test]
    fn a_feasible_point_survives_contraction() {
        // `x = 10`, so `y z = 1234.5678` and `z^2 = y^2 + 99`: a quartic in
        // `y` with one positive root.
        let hard = {
            let product = 1234.5678_f64;
            let y2 = (-99.0 + (99.0_f64 * 99.0 + 4.0 * product * product).sqrt()) / 2.0;
            (y2.sqrt(), (y2 + 99.0).sqrt())
        };
        /// A box, its constraints, and a point known to satisfy them.
        type Case<'a> = (&'a [(&'a str, f64, f64)], &'a [&'a str], &'a [f64]);
        let cases: &[Case<'_>] = &[
            (&[("x", 0.0, 10.0)], &["x > 8"], &[9.0]),
            (
                &[("x1", -2.0, 2.0), ("x2", -2.0, 2.0)],
                &["x1 * x2 == 0 +/- 0.000000001"],
                &[0.0, 1.5],
            ),
            (
                &[("x", -5.0, 5.0)],
                &["(x + 2) * (x - 1) == 0 +/- 0.000000001"],
                &[-2.0],
            ),
            (
                &[("x", -1.0, 1.0), ("y", -1.0, 1.0)],
                &["y == sin(x) +/- 0.000001"],
                &[0.5, 0.5_f64.sin()],
            ),
            (
                &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
                &["x1 == sqrt(x2) +/- 0.000000001", "x1 > 1"],
                &[2.0, 4.0],
            ),
            (
                &[("x", 0.0, 100.0), ("y", 0.0, 100.0), ("z", 0.0, 100.0)],
                &[
                    "x*y*z == 12345.678 +/- 0.001",
                    "x^2 + y^2 == z^2 + 1 +/- 0.001",
                ],
                &[10.0, hard.0, hard.1],
            ),
        ];
        for (variables, sources, point) in cases {
            let inputs = variables
                .iter()
                .map(|(name, lo, hi)| InputVariable::new_continuous(*name, *lo, *hi))
                .collect();
            let system = system(inputs, sources);
            let (result, _) = contracted(&system);
            let Contracted::Live(node) = result else {
                panic!("{sources:?}: {result:?}");
            };
            assert!(
                node.contains(point),
                "{sources:?}: {point:?} fell outside {:?}",
                node.bounds
            );
        }
    }

    /// The parabola ribbon: one contraction cannot separate the roots (the
    /// product's inverse straddles zero), one split can, and each half then
    /// contracts to its band and settles on its centre.
    #[test]
    fn the_parabola_ribbon_leaves_one_settled_box_per_root() {
        let system = system(
            vec![InputVariable::new_continuous("x", -5.0, 5.0)],
            &["(x + 2) * (x - 1) == 0 +/- 0.000000001"],
        );
        let result = pruned(&system, 64);
        let Pruned::Live {
            leaves, exhausted, ..
        } = result
        else {
            panic!("{result:?}");
        };
        assert!(!exhausted);
        let mut centres: Vec<f64> = leaves
            .iter()
            .map(|leaf| {
                leaf.settled
                    .as_ref()
                    .expect("each band settles on its centre")[0]
            })
            .collect();
        centres.sort_by(f64::total_cmp);
        assert_eq!(centres.len(), 2, "{centres:?}");
        assert!((centres[0] + 2.0).abs() < 1e-8 && (centres[1] - 1.0).abs() < 1e-8);
    }

    /// The absolute value's two branches, the same way: the inverse of `abs`
    /// is a hull through zero that one split separates.
    #[test]
    fn the_absolute_value_leaves_one_settled_box_per_branch() {
        let system = system(
            vec![InputVariable::new_continuous("x1", -2.0, 2.0)],
            &["abs(x1) == 1 +/- 0.000000001"],
        );
        let result = pruned(&system, 64);
        let Pruned::Live { leaves, exhausted } = result else {
            panic!("{result:?}");
        };
        assert!(!exhausted);
        let mut centres: Vec<f64> = leaves
            .iter()
            .map(|leaf| {
                leaf.settled
                    .as_ref()
                    .unwrap_or_else(|| panic!("unsettled leaf {:?}", leaf.node.bounds))[0]
            })
            .collect();
        centres.sort_by(f64::total_cmp);
        assert_eq!(centres.len(), 2, "{centres:?}");
        assert!((centres[0] + 1.0).abs() < 1e-8 && (centres[1] - 1.0).abs() < 1e-8);
    }

    /// A contradiction contraction alone cannot see — the inverse of the
    /// square is a hull that straddles zero, so nothing narrows — and one
    /// case split can: on either side of zero the hull is a single interval.
    #[test]
    fn a_disc_inside_a_ring_beyond_it_is_proved_by_splitting() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -1.5, 1.5),
                InputVariable::new_continuous("y", -1.5, 1.5),
            ],
            &["x^2 + y^2 <= 1", "x^2 + y^2 >= 2"],
        );
        let (root, _) = contracted(&system);
        assert!(matches!(root, Contracted::Live(_)), "{root:?}");
        let result = pruned(&system, 256);
        let Pruned::Empty { blamed } = result else {
            panic!("{result:?}");
        };
        assert_eq!(blamed, vec![0, 1]);
    }

    /// The two classes the module doc gives up on stay `Live` and exhaust
    /// the budget rather than claiming anything.
    #[test]
    fn a_thin_or_algebraic_contradiction_is_live_at_the_budget() {
        for sources in [
            &["x + y <= 1", "x + y >= 1.000000001"] as &[&str],
            &["x*x - 2*x*y + y*y < 0"],
        ] {
            let system = system(
                vec![
                    InputVariable::new_continuous("x", -1.0, 1.0),
                    InputVariable::new_continuous("y", -1.0, 1.0),
                ],
                sources,
            );
            let result = pruned(&system, 64);
            let Pruned::Live {
                leaves, exhausted, ..
            } = result
            else {
                panic!("{sources:?}: {result:?}");
            };
            assert!(exhausted, "{sources:?}");
            assert!(
                leaves.iter().all(|leaf| leaf.settled.is_none()),
                "{sources:?}: nothing in an empty region can settle"
            );
        }
    }

    /// A constraint that narrows nothing is what `NotFound` names. A computed
    /// subscript is the permanent case: interval evaluation answers `ENTIRE`
    /// to it whatever the box. The algebraic contradiction, by contrast, does
    /// narrow inside sub-boxes (once `y` has a sign, `x y > 0` gives `x`
    /// one), so it is *not* unexpressed — interval reasoning said something,
    /// just not a proof.
    #[test]
    fn what_narrowed_nothing_is_unexpressed() {
        let subscripted = system(
            vec![
                InputVariable::new_continuous("n", 1.0, 2.0),
                InputVariable::new_continuous("x2", -10.0, 10.0),
            ],
            &["var[floor(n)] < 4", "x2 < 3"],
        );
        let (result, unexpressed) = pruned_with_silence(&subscripted, 16);
        assert!(matches!(result, Pruned::Live { .. }), "{result:?}");
        assert_eq!(unexpressed, vec![0]);

        let algebraic = system(
            vec![
                InputVariable::new_continuous("x", -1.0, 1.0),
                InputVariable::new_continuous("y", -1.0, 1.0),
            ],
            &["x*x - 2*x*y + y*y < 0"],
        );
        let (result, unexpressed) = pruned_with_silence(&algebraic, 16);
        assert!(matches!(result, Pruned::Live { .. }), "{result:?}");
        assert!(unexpressed.is_empty(), "{unexpressed:?}");
    }

    /// The budget is a count of contractions and is honoured to the box:
    /// a bisection at budget `n` performs at most `n` of them.
    #[test]
    fn the_budget_bounds_the_contractions() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -1.0, 1.0),
                InputVariable::new_continuous("y", -1.0, 1.0),
            ],
            &["x*x - 2*x*y + y*y < 0"],
        );
        for budget in [0, 1, 2, 3, 8] {
            let result = pruned(&system, budget);
            let Pruned::Live {
                leaves, exhausted, ..
            } = result
            else {
                panic!("{result:?}");
            };
            // Every contraction that lived is a leaf or was split into two
            // more; either way a budget of `n` leaves at most `n + 1` boxes.
            assert!(
                leaves.len() <= budget as usize + 1,
                "{budget}: {}",
                leaves.len()
            );
            assert!(exhausted, "{budget}");
        }
    }

    /// A chain that climbs a unit a sweep on a box a million wide is stopped
    /// by the progress threshold, not run to the end of the box.
    #[test]
    fn a_zeno_chain_terminates_live() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", 0.0, 1e6),
                InputVariable::new_continuous("y", 0.0, 1e6),
                InputVariable::new_continuous("z", 0.0, 1e6),
            ],
            &["x >= y + 1", "y >= z + 1", "z >= x + 1"],
        );
        let (result, _) = contracted(&system);
        assert!(matches!(result, Contracted::Live(_)), "{result:?}");
    }

    /// The same problem gives the same leaves, in the same order.
    #[test]
    fn a_bisection_is_deterministic() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -1.0, 1.0),
                InputVariable::new_continuous("y", -1.0, 1.0),
            ],
            &["y == sin(x) +/- 0.000001"],
        );
        let bounds = |result: Pruned| -> Vec<Bounds> {
            let Pruned::Live { leaves, .. } = result else {
                panic!("{result:?}");
            };
            leaves.into_iter().map(|leaf| leaf.node.bounds).collect()
        };
        assert_eq!(bounds(pruned(&system, 32)), bounds(pruned(&system, 32)));
    }

    /// `(2.5, 2.9)` holds no integer: rounding the narrowed interval onto
    /// the levels empties it, and the proof blames both bounds.
    #[test]
    fn an_interval_between_two_levels_is_empty() {
        // setup
        let system = system(
            vec![InputVariable::new_discrete("n", 0.0, 1.0, 10.0)],
            &["n > 2.5", "n < 2.9"],
        );

        // act
        let (result, _) = contracted(&system);

        // assert
        let Contracted::Empty { blamed } = result else {
            panic!("expected empty, got {result:?}");
        };
        assert_eq!(blamed, vec![0, 1]);
    }

    /// `%` narrows nothing, so no contraction sees that an even level is
    /// never odd; splitting the lattice down to its eleven points and judging
    /// each one does, inside the budget — and every point that dies blames
    /// the constraint it fails.
    #[test]
    fn a_lattice_split_to_its_points_is_proved_empty() {
        // setup
        let system = system(
            vec![InputVariable::new_discrete("n", 0.0, 2.0, 20.0)],
            &["n % 2 == 1 +/- 0.1"],
        );

        // act
        let result = pruned(&system, 64);

        // assert
        let Pruned::Empty { blamed } = result else {
            panic!("expected empty, got {result:?}");
        };
        assert_eq!(blamed, vec![0]);
    }

    /// A discrete coordinate splits between levels, never through one, so
    /// the pieces of a region one level apart are each a leaf of their own.
    #[test]
    fn each_level_of_a_region_in_pieces_settles_its_own_leaf() {
        // setup
        let system = system(
            vec![
                InputVariable::new("ratio", crate::Domain::Levels(vec![1.0, 2.0, 4.0])),
                InputVariable::new_continuous("speed", 0.0, 100.0),
            ],
            &["speed / ratio >= 20", "speed / ratio <= 20.01"],
        );

        // act
        let result = pruned(&system, 64);

        // assert
        let Pruned::Live { leaves, .. } = result else {
            panic!("expected leaves, got {result:?}");
        };
        let mut ratios: Vec<f64> = leaves
            .iter()
            .filter_map(|leaf| leaf.settled.as_ref().map(|point| point[0]))
            .collect();
        ratios.sort_by(f64::total_cmp);
        ratios.dedup();
        assert_eq!(ratios, vec![1.0, 2.0, 4.0]);
    }
}

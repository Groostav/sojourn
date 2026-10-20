//! A local solve for the search's first point.
//!
//! Finding one feasible point of a nonlinear inequality system is an ordinary
//! constrained optimisation, and a local method eats it: Artemis's SLSQP
//! reaches the stepped beam's optimum from a corner in seconds, where the
//! SMT solver this replaced spent a million resource units per query and
//! brute force cannot find a region a millionth of its box at two hundred
//! variables. This is COBYLA (Powell 1994), derivative-free, with the
//! constraints handled natively by linear models inside a trust region —
//! `basin`'s implementation, pure Rust. It runs after the probe finds
//! nothing and before the box is bisected, so that bisection is reached
//! only when there is nothing to find or pieces to look for.
//!
//! # The objective is the sum of the residuals, and the solve stops at the
//! first feasible point
//!
//! The constraints are COBYLA's to satisfy; the objective only has to give
//! its linear models a slope toward satisfying them from wherever the start
//! is. The sum `Σ g_i(x)` does, everywhere: every coordinate that any
//! constraint names moves it. The worst residual `max_i g_i(x)` — the obvious
//! choice, since descending it is descending toward feasibility — does not:
//! on two hundred bounds `x_i > 10.5` started at the box centre every residual
//! is a hair positive, moving one coordinate fixes one constraint and the
//! maximum does not move, and COBYLA converges on that plateau at a point the
//! oracle rejects, three hundred iterations later. Under the sum the same
//! start lands inside the initial simplex.
//!
//! The solve is not run to any optimum. Every evaluation is a callback of
//! ours, and the moment one is judged feasible the run is stopped: on the
//! stepped beam the first feasible point is evaluation 24 at 100 segments,
//! inside the initial simplex, where converging from there would cost hours,
//! since COBYLA's model algebra grows with the square of the dimension times
//! the constraint count. A seed on the boundary is a seed; the walker's
//! burn-in is what moves away from walls, far more cheaply than the solver.
//!
//! # In the unit cube
//!
//! COBYLA's trust region has one radius for every coordinate, and a box is
//! `[1, 5]` on one coordinate and `[5, 100]` on the next. The solve runs over
//! `u ∈ [0, 1]^d` with `x = lo + u·width`, so a radius means the same on every
//! coordinate of every problem, and the box itself is `2d` more constraints in
//! the same `<= 0` form.
//!
//! # Judged, never trusted
//!
//! Every point the solver evaluates is judged as it goes — the box, every
//! residual `<= 0`, nothing non-finite — and the answer is the judged point
//! with the lowest worst residual seen, never the solver's reported optimum.
//! The one returned is put to [`ConstraintSystem::is_feasible`] once more on
//! the way out, so what leaves here passes the same oracle as every other seed.
//!
//! # Counts, not clocks
//!
//! The starts are the box centre and then draws from the search's seeded rng;
//! each runs for a number of cost evaluations fixed by the dimension. COBYLA
//! is deterministic from a start, so the same seed finds the same point on any
//! machine.
//!
//! # The same solver projects
//!
//! [`nearest`] is the other question a constrained local method answers: the
//! feasible point nearest a given one, `min ‖u − t‖²` over the cube subject
//! to the same rows. It is what `repair` runs where clamping cannot land,
//! from the proposal itself, and it depends on the constraints and nothing
//! else — no anchor, no census, no draw — which is the property an optimizer
//! being repaired needs: the chord it replaced dragged every coordinate
//! toward wherever the census had put points, and steered the optimizer
//! there over thousands of repairs. The objective is squared distance rather
//! than the L1 `repair` reports in, because COBYLA fits linear models and a
//! kink at every axis fights them. Unlike the seed this is run to
//! convergence, since "nearest" is the answer and not "any"; what is kept is
//! still only what was judged, the nearest evaluation with the clearance and
//! the nearest feasible one without it.
//!
//! The rows a projection is given are the constraints **with the clearance
//! built in**: each row is the worst of a constraint's residual at the point
//! and at the point stepped a margin either way along every coordinate the
//! constraint names — the oracle's own neighbourhood, as a number. Without
//! that, a linear constraint defeats the whole thing: COBYLA's steps land
//! exactly on a linear boundary, where a strict comparison's residual is a
//! hair positive, and no evaluation is ever judged feasible. With the margin
//! the boundary COBYLA converges onto is inside the true one by the margin,
//! and what lands there has the clearance with room to spare.

use std::cell::{Cell, RefCell};
use std::convert::Infallible;
use std::rc::Rc;

use basin::core::termination::TerminationReason;
use basin::{Cobyla, CobylaState, CostFunction, Executor, NonlinearInequalityConstraints};
use rand::RngExt;
use rand::rngs::Xoshiro256PlusPlus;

use super::classify;
use super::interval::Interval;
use crate::{ConstraintSystem, Point};

/// How many starts a local solve over the declared box gets before
/// conceding the region: the box centre, then this many minus one draws
/// from the seeded rng.
///
/// Four, because a local method's failure is a bad basin rather than a bad
/// problem, and a fresh start elsewhere is the cheap remedy; more than a few
/// and the solve is doing the sampler's job worse than the sampler. A leaf
/// of a bisection gets one: the leaf *is* the start, a box the contraction
/// has already narrowed to where the region can be.
pub(crate) const STARTS: usize = 4;

/// Cost evaluations a start may spend, per dimension plus one, before it is
/// given up as a bad basin.
///
/// A count, so the same seed finds the same point on every machine, and
/// scaled by the dimension because COBYLA's initial simplex alone is `d + 1`
/// evaluations. Eight of those: on the stepped beam the first feasible point
/// came inside the first simplex at both 20 and 100 segments, so a start that
/// has not landed after eight is not going to. The cap matters only to a start
/// that fails, and a failing start at two hundred variables is minutes of
/// model algebra per simplex-worth — the price of not conceding early, paid
/// rarely.
const EVALS_PER_DIMENSION: u64 = 8;

/// The initial trust-region radius, in the unit cube: a quarter of the box on
/// every coordinate, a coarse first step that the models then shrink.
const INITIAL_RADIUS: f64 = 0.25;

/// The final radius: a start that has shrunk its models this fine without a
/// feasible evaluation has converged somewhere infeasible and is given up.
const FINAL_RADIUS: f64 = 1e-6;

/// The initial trust-region radius of a projection, in the unit cube.
///
/// Smaller than a seed's: a proposal being repaired is usually a hair
/// outside, and the first simplex is evaluated this far along every axis,
/// so a quarter of the box would spend the first `d + 1` evaluations far
/// from anything relevant. A twentieth is still coarse enough that a
/// proposal a tenth of the box outside — the disc's corners in the tests —
/// converges in fifty evaluations.
const PROJECTION_RADIUS: f64 = 0.05;

/// Cost evaluations a projection may spend, per dimension plus one.
///
/// Run to convergence rather than to the first feasible point, so more than
/// a seed's eight: the radius halves from [`PROJECTION_RADIUS`] to
/// [`FINAL_RADIUS`] in about sixteen contractions, each a few evaluations,
/// on top of the initial simplex. Measured to convergence: the disc's corner
/// at 2 dimensions in 50 evaluations, the spring's vertex at 3 in 39 to 47,
/// a proposal a hair outside the ten-segment beam at 20 in 471 — the count
/// grows with the dimension faster than the simplex does, which is why the
/// cap is sixty-four times `d + 1` and not thirty-two. The cap is for a
/// projection that cannot converge, which then answers with the nearest
/// evaluation it did make.
const PROJECTION_EVALS_PER_DIMENSION: u64 = 64;

/// The least margin a projection's rows are shifted inward by, as a fraction
/// of each box width, when the caller asked for no clearance at all.
///
/// A projection asked for feasibility alone would otherwise converge onto the
/// boundary itself, where a strict comparison never passes; this keeps its
/// landings a hair inside, far below anything a caller measuring in the unit
/// cube can see, and the clamp that follows in `repair` lands on the bound
/// exactly where a slice can read it.
const LEAST_MARGIN: f64 = 1e-9;

/// A feasible point of `problem` inside `bounds`, found by local solves from
/// `starts` starts, or `None` when every start ran out of budget without
/// one.
///
/// The unit cube maps onto `bounds`, and the box rows hold the solve inside
/// it: over the declared box that is the search's first point, from
/// [`STARTS`] starts; over a leaf of a bisection it is a piece of the region
/// the solve cannot wander out of, from one. The first start is the centre
/// of `bounds`; `rng` draws the rest.
pub(crate) fn find_initial(
    problem: &ConstraintSystem,
    bounds: &[Interval],
    starts: usize,
    rng: &mut Xoshiro256PlusPlus,
) -> Option<Point> {
    let dimensions = problem.variables.len();
    if dimensions == 0 {
        return None;
    }

    let budget = EVALS_PER_DIMENSION * (dimensions as u64 + 1);
    for start in 0..starts {
        let from: Vec<f64> = if start == 0 {
            vec![0.5; dimensions]
        } else {
            (0..dimensions)
                .map(|_| rng.random_range(0.0..1.0))
                .collect()
        };

        let landing = Landing {
            problem,
            cube: Cube::over(bounds),
            best: RefCell::new(None),
            stop: Rc::new(Cell::new(false)),
            evaluations: Cell::new(0),
        };
        // Before the run, not only after it: a run that never returns leaves
        // this as the last line, which says where and with what.
        tracing::debug!(start, dimensions, budget, "cobyla starts");
        let solver = Cobyla::new()
            .with_initial_radius(INITIAL_RADIUS)
            .with_final_radius(FINAL_RADIUS);
        // The run stops the moment a point is judged feasible: `stop_when`
        // wants a `'static` closure, so the flag it reads is shared with the
        // landing rather than borrowed from it.
        let stop = Rc::clone(&landing.stop);
        // `Infallible` is the error type, so `run` cannot fail; the `Ok` is a
        // type-level formality.
        let Ok(result) = Executor::new(&landing, solver, CobylaState::new(from))
            .max_cost_evals(budget)
            .stop_when(move |_| stop.get().then_some(TerminationReason::UserRequested))
            .run();

        let best = landing.best.into_inner();
        tracing::debug!(
            start,
            evaluations = result.cost_evals(),
            reason = ?result.reason,
            landed = best.is_some(),
            "local solve"
        );
        if let Some((_, point)) = best
            && problem.is_feasible(&point, 0.0)
        {
            tracing::info!(
                start,
                evaluations = result.cost_evals(),
                "a local solve seeded the search"
            );
            return Some(point);
        }
    }
    None
}

/// The unit cube over a box: the coordinates COBYLA works in, `x = lo + u·w`.
struct Cube {
    lower: Vec<f64>,
    width: Vec<f64>,
}

impl Cube {
    fn over(bounds: &[Interval]) -> Self {
        Self {
            lower: bounds.iter().map(|interval| interval.lo()).collect(),
            width: bounds.iter().map(|interval| interval.width()).collect(),
        }
    }

    /// The point in the box that `unit` names.
    fn denormalised(&self, unit: &[f64]) -> Point {
        unit.iter()
            .zip(&self.lower)
            .zip(&self.width)
            .map(|((u, lower), width)| lower + u * width)
            .collect()
    }

    /// Where `point` sits in the cube; a zero-width coordinate is at zero.
    fn normalised(&self, point: &[f64]) -> Vec<f64> {
        point
            .iter()
            .zip(&self.lower)
            .zip(&self.width)
            .map(|((x, lower), width)| {
                if *width > 0.0 {
                    (x - lower) / width
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// The box as constraint rows in the cube's own coordinates: `-u <= 0`
    /// and `u - 1 <= 0`.
    fn rows(unit: &[f64], rows: &mut Vec<f64>) {
        rows.extend(unit.iter().map(|u| -u));
        rows.extend(unit.iter().map(|u| u - 1.0));
    }
}

/// Every constraint's residual at `point`, a fault as `INFINITY` — a point
/// the solver is to step away from, in `basin`'s own convention.
fn residuals(problem: &ConstraintSystem, point: &Point) -> Vec<f64> {
    problem
        .constraints
        .iter()
        .map(|constraint| {
            constraint
                .compiled
                .eval_row(point)
                .map_or(f64::INFINITY, |residual| {
                    if residual.is_finite() {
                        residual
                    } else {
                        f64::INFINITY
                    }
                })
        })
        .collect()
}

/// The seed search as COBYLA sees it: over the unit cube, judged as it goes.
struct Landing<'a> {
    problem: &'a ConstraintSystem,
    cube: Cube,
    /// The judged point with the lowest worst residual so far, with that
    /// residual — normally the first, since the run stops on it, but the stop
    /// is read between iterations and an iteration may evaluate a few more.
    /// Interior mutability because the solver holds the problem by shared
    /// reference and the cost function is, to it, pure.
    best: RefCell<Option<(f64, Point)>>,
    /// Raised when a point has been judged feasible; the executor's stop
    /// check reads it between iterations.
    stop: Rc<Cell<bool>>,
    /// How many times COBYLA has asked, numbering the trace of each ask.
    evaluations: Cell<u64>,
}

impl CostFunction for &Landing<'_> {
    type Param = Vec<f64>;
    type Output = f64;
    type Error = Infallible;

    fn cost(&self, unit: &Vec<f64>) -> Result<f64, Infallible> {
        let point = self.cube.denormalised(unit);
        let total: f64 = residuals(self.problem, &point).iter().sum();

        // Judged here rather than trusted from the solver, and judged with
        // its driven coordinates put on their surfaces: a band a millionth
        // wide is one COBYLA's steps never land in and `centre` lands in
        // every time, so the candidate is the centred point and the cost is
        // still the raw one, which is what the solver's models are of.
        // Inside the box, every residual `<= 0`, nothing non-finite — the
        // oracle's own rule. The first such point ends the run.
        //
        // A discrete coordinate is put on its nearest level first, before
        // the driven ones are centred against it: the solver moves through
        // the relaxation, where the cost has a slope, and what it is judged
        // by is a point the caller's levels allow.
        let mut candidate = point;
        for (value, support) in candidate.iter_mut().zip(&self.problem.supports) {
            *value = support.snap(*value);
        }
        classify::centre(self.problem, &mut candidate);
        let judged = residuals(self.problem, &candidate);
        let worst = judged.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let inside = self
            .problem
            .supports
            .iter()
            .zip(&candidate)
            .all(|(support, value)| support.contains(*value));
        if inside && worst <= 0.0 {
            let mut best = self.best.borrow_mut();
            if best.as_ref().is_none_or(|(deepest, _)| worst < *deepest) {
                *best = Some((worst, candidate));
            }
            self.stop.set(true);
        }
        // Every ask, numbered, at `trace`: what a run was doing when it was
        // last heard from, if it is never heard from again.
        let evaluation = self.evaluations.get() + 1;
        self.evaluations.set(evaluation);
        tracing::trace!(evaluation, cost = total, worst, inside, "cobyla evaluation");
        Ok(total)
    }
}

impl NonlinearInequalityConstraints for &Landing<'_> {
    fn constraints(&self, unit: &Vec<f64>) -> Result<Vec<f64>, Infallible> {
        let mut rows = residuals(self.problem, &self.cube.denormalised(unit));
        Cube::rows(unit, &mut rows);
        Ok(rows)
    }

    fn num_constraints(&self) -> usize {
        self.problem.constraints.len() + 2 * self.problem.variables.len()
    }
}

/// What a projection found: the nearest judged evaluation with the
/// clearance, and the nearest feasible one without it, each with its
/// squared distance to the target in the cube.
pub(crate) struct Projected {
    pub(crate) clear: Option<Point>,
    pub(crate) feasible: Option<Point>,
}

/// The feasible point nearest `target`, sought by one local solve from
/// `from`, over the declared box.
///
/// `from` is where the solve starts — the proposal, or as far as clamping
/// got with it — and `target` is what "nearest" is measured to, in the unit
/// cube. Run to COBYLA's own convergence or the evaluation cap, whichever
/// first; every evaluation is judged where it is made, driven coordinates
/// settled, and only judged points leave. Deterministic: no draw anywhere.
pub(crate) fn nearest(
    problem: &ConstraintSystem,
    from: &[f64],
    target: &[f64],
    clearance: f64,
) -> Projected {
    let _span = tracing::debug_span!("cobyla").entered();
    let dimensions = problem.variables.len();
    let cube = Cube::over(&problem.declared());
    let projection = Projection {
        problem,
        target: cube.normalised(target),
        cube,
        clearance,
        // Twice the clearance, so that a landing on the shifted boundary has
        // the clearance itself with a margin for rounding.
        margin: (2.0 * clearance).max(LEAST_MARGIN),
        clear: RefCell::new(None),
        feasible: RefCell::new(None),
        evaluations: Cell::new(0),
    };
    let start = projection.cube.normalised(from);
    let budget = PROJECTION_EVALS_PER_DIMENSION * (dimensions as u64 + 1);
    tracing::debug!(dimensions, clearance, budget, "cobyla starts");

    let solver = Cobyla::new()
        .with_initial_radius(PROJECTION_RADIUS)
        .with_final_radius(FINAL_RADIUS);
    let Ok(result) = Executor::new(&projection, solver, CobylaState::new(start))
        .max_cost_evals(budget)
        .run();

    let clear = projection.clear.into_inner();
    let feasible = projection.feasible.into_inner();
    tracing::debug!(
        stage = "cobyla",
        evaluations = result.cost_evals(),
        reason = ?result.reason,
        clear = clear.is_some(),
        feasible = feasible.is_some(),
    );
    Projected {
        clear: clear.map(|(_, point)| point),
        feasible: feasible.map(|(_, point)| point),
    }
}

/// The projection as COBYLA sees it: squared distance to the target over
/// the unit cube, the same rows as a seed, judged as it goes.
struct Projection<'a> {
    problem: &'a ConstraintSystem,
    cube: Cube,
    /// What "nearest" is measured to, in the cube.
    target: Vec<f64>,
    clearance: f64,
    /// How far inside the true boundary the rows put COBYLA's, as a fraction
    /// of each box width; see the module doc.
    margin: f64,
    /// The nearest judged evaluation with the clearance, with its squared
    /// distance; and the nearest feasible one without it.
    clear: RefCell<Option<(f64, Point)>>,
    feasible: RefCell<Option<(f64, Point)>>,
    /// How many times COBYLA has asked, numbering the trace of each ask.
    evaluations: Cell<u64>,
}

impl Projection<'_> {
    fn distance(&self, unit: &[f64]) -> f64 {
        unit.iter()
            .zip(&self.target)
            .map(|(u, t)| (u - t) * (u - t))
            .sum()
    }

    /// Every constraint's residual at `point` with the margin built in: the
    /// worst of the residual there and at the point stepped the margin either
    /// way along each coordinate the constraint names. A fault anywhere is
    /// `INFINITY`.
    fn rows(&self, point: &Point) -> Vec<f64> {
        let residual = |constraint: &crate::system::Constraint, at: &[f64]| -> f64 {
            constraint
                .compiled
                .eval_row(at)
                .map_or(f64::INFINITY, |residual| {
                    if residual.is_finite() {
                        residual
                    } else {
                        f64::INFINITY
                    }
                })
        };
        let mut neighbour = point.clone();
        self.problem
            .constraints
            .iter()
            .enumerate()
            .map(|(index, constraint)| {
                let mut worst = residual(constraint, point);
                let id = super::incidence::ConstraintId(index);
                for row in self.problem.incidence.rows_of(id) {
                    let coordinate = row.index();
                    let step = self.margin * self.cube.width[coordinate];
                    if step <= 0.0 {
                        continue;
                    }
                    for sign in [-1.0, 1.0] {
                        neighbour[coordinate] = point[coordinate] + sign * step;
                        worst = worst.max(residual(constraint, &neighbour));
                    }
                    neighbour[coordinate] = point[coordinate];
                }
                worst
            })
            .collect()
    }
}

impl CostFunction for &Projection<'_> {
    type Param = Vec<f64>;
    type Output = f64;
    type Error = Infallible;

    fn cost(&self, unit: &Vec<f64>) -> Result<f64, Infallible> {
        // Judged as a candidate with its driven coordinates settled — what
        // the chord did to its probes — and measured from where that put it.
        let mut point = self.cube.denormalised(unit);
        classify::settle(self.problem, &mut point);
        let distance = self.distance(&self.cube.normalised(&point));
        let keep = |slot: &RefCell<Option<(f64, Point)>>| {
            let mut best = slot.borrow_mut();
            if best.as_ref().is_none_or(|(nearest, _)| distance < *nearest) {
                *best = Some((distance, point.clone()));
            }
        };
        let clear = self.problem.is_feasible(&point, self.clearance);
        if clear {
            keep(&self.clear);
        }
        let feasible = self.problem.is_feasible(&point, 0.0);
        if feasible {
            keep(&self.feasible);
        }
        let evaluation = self.evaluations.get() + 1;
        self.evaluations.set(evaluation);
        tracing::trace!(evaluation, distance, clear, feasible, "cobyla evaluation");
        Ok(self.distance(unit))
    }
}

impl NonlinearInequalityConstraints for &Projection<'_> {
    fn constraints(&self, unit: &Vec<f64>) -> Result<Vec<f64>, Infallible> {
        let mut rows = self.rows(&self.cube.denormalised(unit));
        // The box, shifted inward by the margin too.
        rows.extend(unit.iter().map(|u| self.margin - u));
        rows.extend(unit.iter().map(|u| u + self.margin - 1.0));
        Ok(rows)
    }

    fn num_constraints(&self) -> usize {
        self.problem.constraints.len() + 2 * self.problem.variables.len()
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;
    use crate::InputVariable;
    use crate::system::tests::system;

    fn rng() -> Xoshiro256PlusPlus {
        Xoshiro256PlusPlus::seed_from_u64(0x0001_0CA1)
    }

    #[test]
    fn a_half_space_is_seeded_from_the_centre() {
        let system = system(
            vec![
                InputVariable::new_continuous("x1", -2.0, 2.0),
                InputVariable::new_continuous("x2", -2.0, 2.0),
            ],
            &["2*x1 + x2 < -3"],
        );
        let point = find_initial(&system, &system.declared(), STARTS, &mut rng())
            .expect("a half-space seeds");
        assert!(system.is_feasible(&point, 0.0), "{point:?}");
    }

    #[test]
    fn a_disc_off_centre_is_seeded() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -3.0, 3.0),
                InputVariable::new_continuous("y", -3.0, 3.0),
            ],
            &["sqr(x - 2) + sqr(y - 2) < 0.25"],
        );
        let point =
            find_initial(&system, &system.declared(), STARTS, &mut rng()).expect("a disc seeds");
        assert!(system.is_feasible(&point, 0.0), "{point:?}");
    }

    /// Two discs, and a sub-box holding only one of them: the seed is in the
    /// sub-box, which is the disc the box was asked about and not the one
    /// nearer the declared centre.
    #[test]
    fn a_seed_over_a_sub_box_stays_in_it() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -3.0, 3.0),
                InputVariable::new_continuous("y", -3.0, 3.0),
            ],
            &["min(sqr(x - 2) + sqr(y - 2), sqr(x) + sqr(y)) < 0.25"],
        );
        let bounds = [Interval::new(1.0, 3.0), Interval::new(1.0, 3.0)];
        let point = find_initial(&system, &bounds, 1, &mut rng())
            .expect("the far disc seeds from its own box");
        assert!(system.is_feasible(&point, 0.0), "{point:?}");
        assert!(
            bounds
                .iter()
                .zip(&point)
                .all(|(bound, value)| bound.contains(*value)),
            "{point:?} left {bounds:?}"
        );
    }

    /// A corner of the disc's bounding square projects radially: the nearest
    /// point of the disc to `(1.2, 1.2)` is on the rim at 45 degrees, and the
    /// answer depends on nothing but the constraint.
    #[test]
    fn a_disc_corner_projects_radially() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -2.0, 2.0),
                InputVariable::new_continuous("y", -2.0, 2.0),
            ],
            &["x^2 + y^2 < 1"],
        );
        let corner = [1.2, 1.2];
        let projected = nearest(&system, &corner, &corner, 1e-9);
        let point = projected.clear.expect("the rim has room inside it");
        let angle = point[1].atan2(point[0]).to_degrees();
        let radius = point[0].hypot(point[1]);
        assert!(
            (angle - 45.0).abs() < 0.5,
            "{point:?} is at {angle} degrees"
        );
        assert!(
            (1.0 - radius) < 1e-3 && radius < 1.0,
            "{point:?} is at radius {radius}"
        );
    }

    /// A half-space is entered along its normal: the L2 foot of `(1, 1)` on
    /// `2 x1 + x2 = 1` is `(0.2, 0.6)`, where the axis projection `repair`
    /// clamps to first would move `x1` alone.
    #[test]
    fn a_half_space_projects_along_its_normal() {
        let system = system(
            vec![
                InputVariable::new_continuous("x1", -2.0, 2.0),
                InputVariable::new_continuous("x2", -2.0, 2.0),
            ],
            &["2*x1 + x2 < 1"],
        );
        let outside = [1.0, 1.0];
        let projected = nearest(&system, &outside, &outside, 1e-9);
        let point = projected.clear.expect("a half-space has room");
        assert!(
            (point[0] - 0.2).abs() < 1e-3 && (point[1] - 0.6).abs() < 1e-3,
            "{point:?} is not the foot of the normal"
        );
    }

    /// The same input gives the same point, bit for bit.
    #[test]
    fn a_projection_is_deterministic() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -2.0, 2.0),
                InputVariable::new_continuous("y", -2.0, 2.0),
            ],
            &["x^2 + y^2 < 1"],
        );
        let corner = [-1.2, 1.2];
        let once = nearest(&system, &corner, &corner, 1e-9).clear;
        let twice = nearest(&system, &corner, &corner, 1e-9).clear;
        assert_eq!(once, twice);
    }

    /// An empty region projects to nothing at all, within the budget.
    #[test]
    fn an_empty_region_projects_to_nothing() {
        let system = system(
            vec![InputVariable::new_continuous("x", 0.0, 10.0)],
            &["x > 8", "x < 2"],
        );
        let projected = nearest(&system, &[5.0], &[5.0], 0.0);
        assert!(projected.clear.is_none() && projected.feasible.is_none());
    }

    /// The plateau case: every constraint a hair violated at the centre, so a
    /// worst-residual objective would be flat across the initial simplex. The
    /// sum is not, and the centre start lands inside that simplex.
    #[test]
    fn a_corner_of_many_strict_bounds_is_seeded_from_the_centre() {
        let names: Vec<String> = (1..=40).map(|i| format!("x{i}")).collect();
        let inputs = names
            .iter()
            .map(|name| InputVariable::new_continuous(name.clone(), 10.0, 11.0))
            .collect();
        let sources: Vec<String> = names.iter().map(|name| format!("{name} > 10.5")).collect();
        let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        let system = system(inputs, &sources);
        assert!(
            !system.is_feasible(&vec![10.5; 40], 0.0),
            "the centre is on every wall"
        );

        let point = find_initial(&system, &system.declared(), STARTS, &mut rng())
            .expect("the corner seeds");
        assert!(system.is_feasible(&point, 0.0), "{point:?}");
    }

    #[test]
    fn an_empty_region_is_not_seeded() {
        let system = system(
            vec![InputVariable::new_continuous("x", 0.0, 10.0)],
            &["x > 8", "x < 2"],
        );
        assert!(find_initial(&system, &system.declared(), STARTS, &mut rng()).is_none());
    }

    #[test]
    fn a_faulting_constraint_still_seeds() {
        // `ln` of a negative faults; the fault is a residual the solver steps
        // away from, not an abort.
        let system = system(
            vec![InputVariable::new_continuous("x", -1.0, 3.0)],
            &["ln(x) > 0"],
        );
        let point = find_initial(&system, &system.declared(), STARTS, &mut rng())
            .expect("the log's domain seeds");
        assert!(system.is_feasible(&point, 0.0), "{point:?}");
    }

    #[test]
    fn the_same_seed_finds_the_same_point() {
        let system = system(
            vec![
                InputVariable::new_continuous("x", -3.0, 3.0),
                InputVariable::new_continuous("y", -3.0, 3.0),
            ],
            &["sqr(x - 2) + sqr(y - 2) < 0.25", "x + y > 3.9"],
        );
        let first = find_initial(&system, &system.declared(), STARTS, &mut rng()).expect("seeds");
        let second = find_initial(&system, &system.declared(), STARTS, &mut rng()).expect("seeds");
        assert_eq!(first, second);
    }
}

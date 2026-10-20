//! A feasible point near a given one, deterministically.
//!
//! Reached as [`FeasibleRegion::repair`](crate::FeasibleRegion::repair), which
//! carries the contract; this module is the algorithm, over a
//! `&ConstraintSystem`. It lives on the solved region rather than on the
//! system because a region that could not be solved has nothing to repair
//! toward, and because a caller with a region in hand is the only one who
//! ever asks.
//!
//! The consumer is an optimizer that cannot evaluate its objective at an
//! infeasible point — the constraints encode things like mesh validity, and a
//! bad point is a crashed solver rather than a bad number. Every point it
//! proposes that fails the constraints comes here, thousands of times a run,
//! and must come back feasible by the same oracle the search uses, near where
//! it wanted to be, and the same every time it is asked. The contract and the
//! alternatives it displaced are written up in `docs/todo.md` under *Repair
//! for Artemis*; this is the shape that survived.
//!
//! # The stages, and the order they run in
//!
//! Every stage that lands hands its point to an aggregator; the nearest with
//! the clearance wins, released first (a coordinate reverted to the caller's
//! value can only come nearer). "Nearest" is Euclidean over box-normalised
//! coordinates — the consumer's metric, and human intuition's; the taxicab
//! metric an earlier design measured in preferred a landing that moved one
//! coordinate over a nearer one that moved two, an axis bias an optimizer
//! must not be fed. The stages, cheapest and most exact first:
//!
//! **Clamp** — where cheap. Each coordinate has a conditional slice, the
//! interval it may occupy with the others held ([`hc4::slice`]);
//! clamping into it is the axis projection onto that coordinate's
//! constraints. A slice narrows against every constraint naming the
//! coordinate, so the clamp costs a walk of each such constraint's tree per
//! coordinate per sweep — cheap where constraints are local
//! ([`CLAMP_LOCAL`]), and skipped where one is dense (the beam's deflection
//! names two hundred), which is also where no single-coordinate clamp could
//! land. Where the moved coordinates are *separable* — every constraint
//! naming one names it alone, as bounds do — the axis move is the exact
//! Euclidean projection and is returned outright, on the bound at zero
//! clearance (Artemis measured coordinates *at* a bound as more accurate).
//! Otherwise the landing is a candidate and its reach the derivative-free
//! solve's start.
//!
//! **Newton** — the projection, from the point. Newton on the KKT system
//! over the active set ([`newton::nearest`]), with the gradients the tape's
//! reverse sweep provides: exact in one step on a linear constraint, a few
//! on a curved one, microseconds at fifty variables. It declines where a
//! constraint that bites has no derivative or the iteration will not
//! converge. A clear landing here is the answer — the aggregator returns
//! before the derivative-free stages run at all, so the smooth case pays
//! only Newton. The landing depends on the constraints and the point and on
//! nothing else, which is the property the consumer needs and the one an
//! earlier design lacked: it chorded from *anchors* the caller supplied, so
//! every landing was dragged toward the census — a disc corner at 45° landed
//! at 17°, where a projection lands it at 45°.
//!
//! **Newton from the reference's chord** — where Newton from the point
//! declined on a constraint that has gradients: flat where the point stands,
//! or too curved there for the linearisation to hold. A chord bisected from
//! the region's reference to the point lands on the boundary where the
//! constraint is well-scaled again, and Newton from *there*, toward the
//! point, is the same projection with a start it can read from — the
//! step-shortening in [`newton`] walks it in. Keane's bump at a hundred
//! variables with eleven coordinates at zero: the product's gradient is
//! exactly zero at the point, and this lands the true nearest point, the
//! zeros lifted to `3.7e-6`, in fourteen iterations — where the box and two
//! COBYLA runs below took five seconds and landed forty percent farther.
//! Cheap enough to run before them: a chord is sixty evaluations.
//!
//! **Sample and the derivative-free solve** — off the smooth path, where
//! Newton declined: a jump (`floor`, `%`, `sgn`) or a curvature Newton could
//! not settle. The sampling box first — uniform draws around the point,
//! driven coordinates on their surfaces, the box doubled while empty and
//! shrunk onto the nearest hit, then a chord from that hit — because it
//! needs no slice, gradient or continuity, only a region fat enough to
//! sample over its free coordinates, which is what a jump leaves. Then
//! COBYLA ([`local::nearest`]), where the box found nothing *or* the
//! dimension is small ([`COBYLA_CHEAP`]): the box shrinks onto the first
//! branch it meets and can miss a nearer one on a thin many-branched region
//! (`(x+2)(x-1) == 0`), where COBYLA's linear models on a smooth constraint
//! do not, and there it is cheap; past a handful of dimensions the regions
//! that reach here are fat ones the box handles and COBYLA is the last
//! resort, `O(d²m)` an iteration. The box across a jump: 0.40 of the box
//! from the proposal became the nearest cell (`tests/cvg_repair.rs`, the ball
//! oracle across a jump).
//!
//! **Reference** — nothing feasible seen at all: a constraint flat where the
//! point stands (Keane's `0.75 − ∏xᵢ` with a coordinate at `1e-11` is `0.75`
//! to fifty digits every way) *and* without a gradient Newton could use,
//! which no slice, model or box around the point reads. The same chord from
//! the region's reference — a local solve from the box centre that `solve`
//! ran once, a function of the system and not of anything sampled since —
//! and the derivative-free solve from its landing, where the constraint is
//! well-scaled again; so the reference decides only which basin, never where
//! in it. `Stranded` means this found nothing either.
//!
//! A boundary landing — a chord's, or a Newton one — is stepped inside by
//! [`stepped_inside`] / [`off_the_boundary`]: along the inward direction in
//! hand (Newton's wedge bisector, or the chord's own), and failing that the
//! clamp, in doublings of the clearance until the oracle passes.
//!
//! # Clearance: a deliberate step inside, not an ulp
//!
//! The caller stores points normalised to a unit cube and denormalises them
//! before evaluating. That round trip is one ulp off for some values, and one
//! ulp on a coordinate can move a residual by `1e-8` when the coordinate enters
//! a term at the fourth power. A landing a few ulps inside a bound — which is
//! what the ladder alone produces — is feasible here and infeasible by the
//! time the caller looks at it. Found at a vertex of the tension spring, where two constraints
//! are active at once; `tests/regression_fixture.rs` keeps the case.
//!
//! So `repair` takes a **clearance**: a fraction of each variable's box width
//! that the answer keeps between itself and every wall, in every coordinate
//! direction. One scalar, but `clearance * width` on each coordinate, which is
//! the per-axis tolerance the box's own scale implies, in the unit cube the
//! caller normalises into and the metric this module measures distance in. A
//! point *has* the clearance when it is feasible and so are its `2d` axis
//! neighbours at that distance — [`ConstraintSystem::is_feasible`] with the
//! clearance, which judges each constraint at the point and along each
//! variable it names — which is exactly "survives any per-coordinate
//! perturbation smaller than the step", and on a convex region is the whole
//! L1 ball of that radius.
//!
//! The oracle judges; the slices construct. A clamp lands inside the slice
//! shrunk by the step at each end, and a slice is an outer enclosure, so
//! "inside the shrunk slice" is where to aim and never a proof. A point that
//! truly has the clearance sits inside every shrunk slice, though, so the clamp
//! never moves a point that already qualifies. Where a slice is narrower than
//! twice the step the clamp aims for its middle, and if nothing reached has the
//! clearance the answer is [`RepairError::Cramped`] with the nearest feasible
//! point in it: the region is thinner than the caller's own noise floor, and
//! saying so beats handing back a point that will fail on the next look.
//!
//! # The metric is Euclidean over box-normalised coordinates
//!
//! Normalised, because the caller thinks in a unit cube and "near" in metres
//! and pascals at once means nothing. Euclidean, because that is the
//! consumer's question and human intuition's: the taxicab metric this used to
//! measure in prefers any landing that touches one coordinate over a nearer
//! one that touches two, which is a bias toward the axes an optimizer should
//! not be fed. Taxicab was chosen for ranking anchors, where the contrast
//! between nearest and farthest neighbour collapses in high dimension and
//! collapses fastest for the higher norms (Aggarwal, Hinneburg & Keim, 2001);
//! the anchors are gone and the argument went with them. Candidates are
//! ranked, the consolation chosen and the back-off's unit measured in the
//! same metric the projection minimises.
//!
//! # What is deliberately not here
//!
//! No randomness the caller can see: the sampling box draws from a clone of
//! the stream the region keeps, so the answer is a function of the region,
//! the point and the clearance. No finite differences: the
//! gradients Newton reads are the tape's own reverse sweep, exact, and a
//! constraint without one declines rather than being approximated at a
//! vertex where an approximation is ill-defined. No anchors: nothing the
//! caller has seen elsewhere may influence where a point lands, for the
//! reason above. No brute force *first*: it cannot localise a thin region
//! or a high-dimensional one, and on the cases the projection handles it
//! would re-solve the find-a-first-point problem on every call; it is the
//! last stage, for the cases nothing else can read.

use rand::RngExt;
use rand::rngs::Xoshiro256PlusPlus;

use crate::cvg::incidence::{ConstraintId, Row};
use crate::cvg::{classify, hc4, local, newton, normalised_distance as distance};
use crate::{ConstraintSystem, Point};

/// How many rounds of clamping a point gets before the projection starts
/// from wherever it got to.
///
/// A clamp is computed with the other coordinates held, so a coordinate that
/// moves can open room for another; a few rounds catch that. Most points land
/// in one, and a point that has not landed after this many is one the slices
/// do not describe — a gap or a corner the projection has to find its way
/// round.
const CLAMP_SWEEPS: usize = 8;

/// The most coordinates a constraint may name for the clamp to be run.
///
/// A slice narrows against every constraint naming its coordinate, so the
/// clamp's cost per sweep is a walk of each such constraint's tree per
/// coordinate. A constraint naming this few is cheap to clamp against; one
/// naming many — the beam's deflection names two hundred — is not, and is
/// also one no single-coordinate clamp can land on, so the clamp is skipped
/// there and the projection does the work. Sixteen: a wide constraint, but
/// far short of the dense ones where the cost bites.
const CLAMP_LOCAL: usize = 16;

/// The most dimensions at which the derivative-free solve runs as a cross-
/// check beside the sampling box, rather than only when the box fails.
///
/// COBYLA costs about `d²m` an iteration, so it is cheap here and dear past
/// it; and here is where the box is weakest — it cannot localise a thin,
/// many-branched region and shrinks onto the first branch it meets, where
/// COBYLA on a smooth constraint finds the nearest. Past this the regions
/// that reach the box are fat ones it handles, and COBYLA is the last resort.
const COBYLA_CHEAP: usize = 8;

/// Bisection steps along the chord from a reference point. Each halves the
/// bracket, so this is a budget in bits and sixty is past where an `f64`
/// parameter in `[0, 1]` can still be halved.
const CHORD_BITS: usize = 60;

/// Rounds the sampling box gets: each doubles the box when it held nothing
/// and shrinks it onto the nearest hit when it did.
///
/// The box starts at a sixty-fourth of every width and doubles to the whole
/// declared box in six rounds; the rest shrink. Twenty is far past where
/// shrinking stops finding nearer points at this many draws.
const SAMPLING_ROUNDS: usize = 20;

/// Draws per round of the sampling box, judged one by one.
const SAMPLING_DRAWS: usize = 256;

/// Where the sampling box starts, as a fraction of every width.
const SAMPLING_RADIUS: f64 = 1.0 / 64.0;

/// How far inward a landing may nudge, as a power of two in ulps.
///
/// A slice is padded outward by a few ulps per operator, so a clamp onto its
/// edge lands a hair outside. The nudge doubles from one ulp; twenty doublings
/// is a relative `1e-10`, which is well past any padding a real expression
/// accumulates and still nothing next to the tolerances constraints carry.
const LANDING_LADDER: u32 = 20;

/// Why [`FeasibleRegion::repair`](crate::FeasibleRegion::repair) could not answer.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RepairError {
    /// Nothing feasible was reached: clamping could not land, and the
    /// projection found no feasible evaluation within its budget — a region
    /// too thin for a local solve to fall into, or none at all.
    #[error(
        "no feasible point was reached: clamping could not land and the projection found nothing"
    )]
    Stranded,
    /// A feasible point was reached, but nowhere with the clearance asked for:
    /// the feasible room there is narrower than twice the clearance. `nearest`
    /// is that point, feasible by the plain oracle, in case it is better than
    /// nothing; a smaller clearance is the usual answer.
    #[error(
        "a feasible point was reached but none with clearance {clearance}; the feasible room is          narrower than the clearance asked for, so try a smaller one"
    )]
    Cramped { nearest: Point, clearance: f64 },
}

/// A point that satisfies `system` with `clearance` to spare, near `point`,
/// the same every time; `reference` is a feasible point of the system, as
/// far inside as the region has, that the flat case walks in from. The
/// contract — clearance, the guarantees, the errors and the panics — is
/// documented on [`FeasibleRegion::repair`](crate::FeasibleRegion::repair),
/// the only caller.
pub(crate) fn repair(
    system: &ConstraintSystem,
    reference: &[f64],
    point: &[f64],
    clearance: f64,
    stream: Xoshiro256PlusPlus,
) -> Result<Point, RepairError> {
    let dimensions = system.variables().len();
    assert_eq!(
        point.len(),
        dimensions,
        "a point has one coordinate per variable of the system it is repaired against"
    );
    assert!(
        clearance.is_finite() && clearance >= 0.0,
        "a clearance is a non-negative fraction of the box width, not {clearance}"
    );

    let _span = tracing::debug_span!("repair", dimensions, clearance).entered();
    let current: Point = point.to_vec();
    if system.is_feasible(&current, clearance) {
        tracing::debug!(stage = "unchanged", "already has the clearance");
        return Ok(current);
    }

    let widths = crate::cvg::widths(system);

    // Every landing any stage produced, labelled by the stage; the nearest
    // with the clearance is the answer, the nearest feasible one without it
    // the consolation.
    let mut landings: Vec<(&'static str, Landing)> = Vec::new();

    // The clamp, where it is cheap. A slice narrows against every constraint
    // naming the coordinate, so a constraint that names many coordinates
    // costs a walk of its whole tree per coordinate per sweep — ninety
    // milliseconds at two hundred variables under one dense constraint — and
    // that same constraint is one no single-coordinate clamp can satisfy, so
    // the work is spent only to fail. Where every constraint is local the
    // clamp is cheap, and where it is *separable* it is the exact Euclidean
    // projection (the axis move is the whole answer), landed on the bound and
    // returned outright. Otherwise its landing is a candidate and its reach
    // the derivative-free solve's start.
    let clamp_cheap = (0..system.constraints.len())
        .all(|index| system.incidence.rows_of(ConstraintId(index)).len() <= CLAMP_LOCAL);
    let start = if clamp_cheap {
        match clamped(system, &widths, current.clone(), clearance) {
            Ok(landed) => {
                let separable = landed
                    .iter()
                    .zip(point)
                    .enumerate()
                    .filter(|(_, (moved, was))| moved != was)
                    .all(|(coordinate, _)| {
                        system
                            .incidence
                            .naming(Row(coordinate))
                            .iter()
                            .all(|id| system.incidence.rows_of(*id).len() == 1)
                    });
                if separable {
                    tracing::debug!(stage = "clamp", separable = true, "landed");
                    return Ok(released(system, landed, point, clearance));
                }
                tracing::debug!(stage = "clamp", landed = true);
                landings.push(("clamp", Landing::Clear(landed.clone())));
                landed
            }
            Err(reached) => {
                let feasible = system.is_feasible(&reached, 0.0);
                tracing::debug!(stage = "clamp", landed = false, feasible);
                if feasible {
                    landings.push(("clamp", Landing::Feasible(reached.clone())));
                }
                reached
            }
        }
    } else {
        current
    };

    // Newton on the KKT system, from the point, wherever every violated
    // constraint has a gradient: exact where it converges, and where a dense
    // constraint made the clamp too expensive to run this is the only
    // projection there is.
    let differentiable = system.constraints.iter().all(|constraint| {
        constraint.compiled.gradient().is_some()
            || constraint
                .compiled
                .eval_row(point)
                .is_ok_and(|residual| residual <= 0.0)
    });
    if differentiable
        && let Some(landing) = newton::nearest(system, point, point)
        && let Some(landing) = stepped_inside(system, &widths, landing, point, clearance)
    {
        landings.push(("newton", landing));
    }
    if landings.iter().any(|(_, landing)| landing.is_clear()) {
        return nearest_of(system, &widths, landings, point, clearance);
    }

    // Newton again, from the reference's chord: the constraint is flat or
    // too curved where the point stands, and the chord's landing is where it
    // is well-scaled. A candidate, not the answer — the reference decides
    // which basin the chord lands in, and a nearer one may be the box's to
    // find (`(x+2)(x-1) == 0` from 0.9, with the reference in the basin at
    // -2). Computed once, here or below, whichever asks first.
    let mut chord: Option<Point> = None;
    if differentiable {
        let from = chord.insert(along_chord(system, reference, point));
        if let Some(landing) = newton::nearest(system, from, point)
            && let Some(landing) = stepped_inside(system, &widths, landing, point, clearance)
        {
            landings.push(("chord", landing));
        }
    }

    // Off the smooth path — a constraint with a jump in it (`floor`, `%`,
    // `sgn`), or one too curved for Newton to converge on. The sampling box
    // around the point first: cheap, blind to jumps, needs only a region fat
    // enough to sample; then the derivative-free solve, only where the box
    // found nothing, since its landing may be anywhere its evaluations fell
    // feasible and it costs a hundred times the box. See the module doc.
    {
        let projected = landings.iter().any(|(_, landing)| landing.is_clear());
        let hit = sampled(system, &widths, point, clearance, stream);
        if let Some(hit) = &hit {
            let chord = along_chord(system, hit, point);
            landings.extend(
                off_the_boundary(system, &widths, chord, point, clearance)
                    .map(|landing| ("sample", landing)),
            );
        }
        // The derivative-free solve: where the box found nothing it is the
        // last resort, and in low dimension it is a cheap cross-check the box
        // needs — the box shrinks onto the first band it finds and can miss a
        // nearer one on a thin, many-branched region (`(x+2)(x-1) == 0`),
        // where COBYLA's linear models, on a smooth constraint, do not. Past
        // a handful of dimensions it is neither cheap nor better than the box
        // on the fat regions that reach here, so it waits for the box to fail
        // — and where the chord's Newton already projected, a box that found
        // nothing is no reason to spend it: at a hundred variables it is
        // seconds, and it landed farther than the projection where measured.
        if (hit.is_none() && !projected) || dimensions <= COBYLA_CHEAP {
            // Above the cheap dimension this is the rung that makes a repair
            // seconds rather than microseconds, and every arrival so far has
            // been a projection that should have landed: a warning, with the
            // point, so the caller's log names the case to file.
            if dimensions > COBYLA_CHEAP {
                tracing::warn!(
                    dimensions,
                    point = ?point,
                    "repair fell through to the derivative-free solve; a projection should have landed here"
                );
            }
            landings.extend(
                stepped_off(
                    system,
                    &widths,
                    local::nearest(system, &start, point, clearance),
                    point,
                    clearance,
                )
                .into_iter()
                .map(|landing| ("cobyla", landing)),
            );
        }
    }

    // Nothing feasible seen at all: a constraint flat where the point
    // stands, which no slice, no linear model and no box around the point
    // can read. Walk in from the reference instead — see the module doc.
    if landings.is_empty() {
        tracing::debug!(stage = "reference");
        let chord = chord.unwrap_or_else(|| along_chord(system, reference, point));
        landings.extend(
            stepped_off(
                system,
                &widths,
                local::nearest(system, &chord, point, clearance),
                point,
                clearance,
            )
            .into_iter()
            .map(|landing| ("reference", landing)),
        );
        // The projection could not leave the chord's landing either — flat
        // again — so the landing itself, stepped off its walls, stands.
        if !landings.iter().any(|(_, landing)| landing.is_clear()) {
            landings.push((
                "reference",
                match clamped(system, &widths, chord.clone(), clearance) {
                    Ok(clear) => Landing::Clear(clear),
                    Err(_) => Landing::Feasible(chord),
                },
            ));
        }
    }

    nearest_of(system, &widths, landings, point, clearance)
}

/// The aggregator: every candidate released — reverting a coordinate to the
/// caller's value can only bring it nearer — and the Euclidean-nearest with
/// the clearance is the answer, the nearest feasible one without it the
/// consolation.
fn nearest_of(
    system: &ConstraintSystem,
    widths: &[f64],
    landings: Vec<(&'static str, Landing)>,
    point: &[f64],
    clearance: f64,
) -> Result<Point, RepairError> {
    let mut best: Option<(f64, Point)> = None;
    let mut cramped: Option<(f64, Point)> = None;
    let mut winner = "none";
    for (stage, landing) in landings {
        match landing {
            Landing::Clear(candidate) => {
                let candidate = released(system, candidate, point, clearance);
                let at = distance(widths, &candidate, point);
                tracing::debug!(stage, clear = true, at);
                if best.as_ref().is_none_or(|(nearest, _)| at < *nearest) {
                    winner = stage;
                }
                nearer(&mut best, at, candidate);
            }
            Landing::Feasible(candidate) => {
                let at = distance(widths, &candidate, point);
                tracing::debug!(stage, clear = false, at);
                nearer(&mut cramped, at, candidate);
            }
        }
    }

    match (best, cramped) {
        (Some((at, landed)), _) => {
            tracing::debug!(winner, at, "repaired");
            Ok(landed)
        }
        (None, Some((at, nearest))) => {
            tracing::debug!(at, "cramped");
            Err(RepairError::Cramped { nearest, clearance })
        }
        (None, None) => {
            tracing::debug!("stranded");
            Err(RepairError::Stranded)
        }
    }
}

/// What a stage of `repair` hands up: a point with the clearance, or one
/// that is feasible without it.
enum Landing {
    Clear(Point),
    Feasible(Point),
}

impl Landing {
    fn is_clear(&self) -> bool {
        matches!(self, Self::Clear(_))
    }
}

/// Keeps `candidate` in `slot` if it is nearer than what is there.
fn nearer(slot: &mut Option<(f64, Point)>, at: f64, candidate: Point) {
    if slot.as_ref().is_none_or(|(nearest, _)| at < *nearest) {
        *slot = Some((at, candidate));
    }
}

/// The landings a derivative-free projection yields, once its points have
/// been stepped off their walls: at most one with the clearance and one
/// without.
///
/// A `clear` landing gets the clamp from a feasible point, which only steps a
/// coordinate off a wall it is against and leaves one with room alone. A
/// `feasible` landing without the clearance is a landing on the boundary,
/// and goes the way every such landing goes: [`off_the_boundary`].
fn stepped_off(
    system: &ConstraintSystem,
    widths: &[f64],
    projected: local::Projected,
    point: &[f64],
    clearance: f64,
) -> Vec<Landing> {
    let mut landings = Vec::new();
    if let Some(landing) = projected.clear {
        let clear = match clamped(system, widths, landing.clone(), clearance) {
            Ok(clear) => clear,
            Err(_) => landing,
        };
        landings.push(Landing::Clear(clear));
    }
    if let Some(landing) = projected.feasible {
        landings.extend(off_the_boundary(system, widths, landing, point, clearance));
    }
    landings
}

/// A Newton landing stepped inside: along the wedge's bisector, which the
/// projection hands over with the point, then the ways every other landing
/// goes.
fn stepped_inside(
    system: &ConstraintSystem,
    widths: &[f64],
    landing: newton::Landing,
    point: &[f64],
    clearance: f64,
) -> Option<Landing> {
    if let Some(clear) = backed_off(system, widths, &landing.point, &landing.inward, clearance) {
        return Some(Landing::Clear(clear));
    }
    off_the_boundary(system, widths, landing.point, point, clearance)
}

/// A landing on the boundary — a chord's, or a derivative-free one judged
/// feasible without the clearance — stepped inside.
///
/// The ladder first, along `point -> landing` continued past the landing:
/// that direction is `−Σ λᵢ ∇gᵢ`, the boundary's normal at a projection's
/// landing, and the chord's own direction at a chord's — the one inward
/// direction in hand where no slice can read a wall. Then the clamp, which
/// from a feasible point is precisely "step each coordinate its clearance
/// off the nearest wall" — and costs a narrowing per coordinate per
/// constraint, which is why it is second. Where nothing passes, the
/// consolation is the clamp's own result if it stayed feasible — the middle
/// of a slab beats its edge — and the landing itself if that is; a landing a
/// hair outside with nothing inside it is no landing at all.
fn off_the_boundary(
    system: &ConstraintSystem,
    widths: &[f64],
    landing: Point,
    point: &[f64],
    clearance: f64,
) -> Option<Landing> {
    let _span = tracing::debug_span!("off_the_boundary").entered();
    let inward: Vec<f64> = landing
        .iter()
        .zip(point)
        .map(|(to, from)| to - from)
        .collect();
    if let Some(clear) = backed_off(system, widths, &landing, &inward, clearance) {
        return Some(Landing::Clear(clear));
    }
    let reached = match clamped(system, widths, landing.clone(), clearance) {
        Ok(clear) => return Some(Landing::Clear(clear)),
        Err(reached) => reached,
    };
    if system.is_feasible(&reached, 0.0) {
        Some(Landing::Feasible(reached))
    } else if system.is_feasible(&landing, 0.0) {
        Some(Landing::Feasible(landing))
    } else {
        None
    }
}

/// The ladder: `landing` moved along `inward` in doublings, from the
/// clearance — or from an ulp's worth of the box where none was asked for,
/// since a landing on a strict boundary is a hair outside it — until the
/// oracle passes, or the move would be a whole box. The first rung that
/// passes is within a factor of two of the least back-off, which at this
/// scale is all the precision the answer can use.
fn backed_off(
    system: &ConstraintSystem,
    widths: &[f64],
    landing: &[f64],
    inward: &[f64],
    clearance: f64,
) -> Option<Point> {
    let length = distance(widths, inward, &vec![0.0; inward.len()]);
    if length.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let mut rung = clearance.max(f64::EPSILON) / length;
    while rung < 1.0 {
        let mut probe: Point = landing
            .iter()
            .zip(inward)
            .map(|(at, direction)| at + rung * direction)
            .collect();
        classify::settle(system, &mut probe);
        if system.is_feasible(&probe, clearance) {
            return Some(probe);
        }
        rung *= 2.0;
    }
    None
}

/// The nearest feasible point the sampling box finds around `point`: uniform
/// draws in a box centred on it, the box doubled while it holds nothing and
/// shrunk onto the nearest hit once it does, for a fixed number of rounds.
///
/// `rng` is the region's own stream, a clone the caller made: the same
/// landing on every call, on every machine, and a landing that depended on
/// what had been sampled since would carry the bias the anchors had and the
/// reason they are gone.
///
/// The backstop that needs no slice, no gradient and no continuity — only
/// that the region be fat enough to sample *over the free coordinates*: a
/// driven equality is not sampled but computed, every draw's driven
/// coordinates put at the middle of their bands, so a band a millionth
/// wide costs nothing. Draws are judged with the clearance. A hit is the box's answer;
/// the chord from it toward the point refines along that one ray. Nothing
/// past a handful of dimensions localises this way, and it is reached only
/// when every stage that could has declined.
fn sampled(
    system: &ConstraintSystem,
    widths: &[f64],
    point: &[f64],
    clearance: f64,
    mut rng: Xoshiro256PlusPlus,
) -> Option<Point> {
    let _span = tracing::debug_span!("sample").entered();
    let mut radius = SAMPLING_RADIUS;
    let mut best: Option<(f64, Point)> = None;
    for _ in 0..SAMPLING_ROUNDS {
        let mut found = false;
        for _ in 0..SAMPLING_DRAWS {
            let mut draw: Point = point
                .iter()
                .zip(widths)
                .zip(system.variables())
                .map(|((centre, width), variable)| {
                    let step = radius * width;
                    rng.random_range(centre - step..=centre + step)
                        .clamp(variable.lower(), variable.upper())
                })
                .collect();
            // Driven coordinates onto their surfaces, at the middle of the
            // band: a clamp to its edge lands an ulp outside and every draw
            // fails, which is how a jump beside a driven equality stranded.
            classify::centre(system, &mut draw);
            if !system.is_feasible(&draw, clearance) {
                continue;
            }
            found = true;
            let at = distance(widths, &draw, point);
            if best.as_ref().is_none_or(|(nearest, _)| at < *nearest) {
                best = Some((at, draw));
            }
        }
        radius = match &best {
            // Shrink onto the nearest hit: it sits on the box's boundary.
            Some((nearest, _)) if found => *nearest,
            Some((nearest, _)) => nearest.min(radius),
            None => (radius * 2.0).min(1.0),
        };
    }
    tracing::debug!(
        stage = "sample",
        hit = best.is_some(),
        radius,
        rounds = SAMPLING_ROUNDS,
        draws = SAMPLING_ROUNDS * SAMPLING_DRAWS
    );
    best.map(|(_, hit)| hit)
}

/// The last feasible point along the segment from `reference` to `point`,
/// by bisection: `t = 0` is the reference and feasible, `t = 1` is the point
/// and is not, and the bracket halves toward wherever feasibility ends, with
/// driven coordinates settled at every probe. A segment through a gap is
/// bracketed just the same — the feasible end is always a probe that passed,
/// or the reference itself.
fn along_chord(system: &ConstraintSystem, reference: &[f64], point: &[f64]) -> Point {
    let _span = tracing::debug_span!("chord").entered();
    let mut landed: Point = reference.to_vec();
    let (mut lower, mut upper) = (0.0_f64, 1.0_f64);
    for _ in 0..CHORD_BITS {
        let middle = 0.5 * (lower + upper);
        if middle <= lower || middle >= upper {
            break;
        }
        let mut probe: Point = reference
            .iter()
            .zip(point)
            .map(|(from, to)| from + middle * (to - from))
            .collect();
        classify::settle(system, &mut probe);
        if system.is_feasible(&probe, 0.0) {
            lower = middle;
            landed = probe;
        } else {
            upper = middle;
        }
    }
    landed
}

/// `from` clamped into its slices, sweep by sweep, until it has the clearance:
/// `Ok` with that point, `Err` with the point as far as clamping got.
///
/// Called on the caller's point and on the projection's landing. From an
/// infeasible point it is the axis projection described in the module doc; from a
/// feasible one it is the step off the walls, since a coordinate already
/// inside its shrunk slice is not moved.
fn clamped(
    system: &ConstraintSystem,
    widths: &[f64],
    from: Point,
    clearance: f64,
) -> Result<Point, Point> {
    let _span = tracing::debug_span!("clamp").entered();
    let mut current = from;
    for _ in 0..CLAMP_SWEEPS {
        // Every coordinate's clamp, with what it costs, from the point as it
        // stands at the start of the sweep. Cost is measured before any are
        // applied, which is what makes "cheapest first" a statement about the
        // point rather than about the order the coordinates happen to be in.
        let mut clamps: Vec<(f64, usize, f64)> = (0..current.len())
            .filter_map(|coordinate| {
                let slice = hc4::slice(system, &current, coordinate);
                if slice.is_empty() {
                    return None;
                }
                let value = current[coordinate];
                // Aim the clearance inside each edge of the slice; where the
                // slice has no room for that, aim for its middle and let the
                // oracle say whether that was enough.
                let step = clearance * widths[coordinate];
                let (lo, hi) = (slice.lo() + step, slice.hi() - step);
                let target = if lo <= hi {
                    value.clamp(lo, hi)
                } else {
                    0.5 * (slice.lo() + slice.hi())
                };
                if target == value {
                    return None;
                }
                let cost = if widths[coordinate] > 0.0 {
                    (target - value).abs() / widths[coordinate]
                } else {
                    0.0
                };
                Some((cost, coordinate, target))
            })
            .collect();
        if clamps.is_empty() {
            break;
        }
        clamps.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

        for (_, coordinate, target) in clamps {
            let value = current[coordinate];
            current[coordinate] = target;

            // The slice's edge is a hair outside the true bound, so step inward
            // — toward where the value came from is *outward* — until the
            // constraints naming this coordinate pass, with the clearance:
            // a landing `step` inside the padded edge is a hair short of
            // `step` inside the wall, and its neighbours tie with the wall.
            // `landed` is exactly that check; it is a filter over one
            // coordinate's constraints and never the judge, which the full
            // check below remains.
            //
            // The ulp is the box width's, not the value's. A bound at zero has
            // ulps of `5e-324`, and a strict comparison is satisfied only past
            // an absolute `f64::MIN_POSITIVE`, which no ladder of those reaches;
            // the width is the scale the residual's own rounding lives at.
            if !landed(system, &current, coordinate, clearance) {
                let inward = if target > value { 1.0 } else { -1.0 };
                let magnitude = target.abs().max(widths[coordinate]).max(f64::MIN_POSITIVE);
                let ulp = magnitude.next_up() - magnitude;
                for rung in 0..LANDING_LADDER {
                    let nudged = target + inward * ulp * f64::from(1u32 << rung);
                    current[coordinate] = nudged;
                    if landed(system, &current, coordinate, clearance) {
                        break;
                    }
                    current[coordinate] = target;
                }
            }

            if system.is_feasible(&current, clearance) {
                return Ok(current);
            }
        }
    }
    // Nothing left to clamp, or the sweeps ran out. A point with nothing to
    // clamp may already have the clearance — a projection's landing that met
    // no wall — and is judged rather than presumed.
    if system.is_feasible(&current, clearance) {
        Ok(current)
    } else {
        Err(current)
    }
}

/// Whether the constraints naming `coordinate` hold at `point` with the
/// clearance, and the box does on that coordinate.
///
/// The clamp's filter, never its judge: mid-clamp the rest of the system may
/// still be broken, and the question is only whether *this* landing is on the
/// right side of the walls it can see. A constraint naming none of the
/// coordinates that moved is not consulted; a computed subscript counts as
/// naming every coordinate, which the incidence graph carries.
fn landed(system: &ConstraintSystem, point: &Point, coordinate: usize, clearance: f64) -> bool {
    if !system.within(point, coordinate, clearance) {
        return false;
    }
    let mut neighbour = point.clone();
    system.incidence.affected(Row(coordinate)).iter().all(|id| {
        let rows = system.incidence.rows_of(*id);
        system.constraints[id.index()].holds(point, rows, &system.grids, clearance, &mut neighbour)
    })
}

/// `candidate` with every coordinate put back to its value in `original`
/// wherever that keeps the clearance, one coordinate at a time in schema
/// order.
///
/// Clear in, clear out: a reversion is kept only if the whole point still
/// passes with its clearance, so this can only shorten the distance to
/// `original`, never break the answer. It exists because both stages over-move
/// — a clamp is computed against neighbours that then move too, and a
/// projection carries every coordinate when one constraint was active — and hindsight is
/// one check per coordinate. The clearance is part of the check because a
/// coordinate the caller left an ulp from a wall is exactly what must not be
/// handed back.
fn released(
    system: &ConstraintSystem,
    mut candidate: Point,
    original: &[f64],
    clearance: f64,
) -> Point {
    let _span = tracing::debug_span!("release").entered();
    for coordinate in 0..candidate.len() {
        let moved = candidate[coordinate];
        if moved == original[coordinate] {
            continue;
        }
        candidate[coordinate] = original[coordinate];
        if !system.is_feasible(&candidate, clearance) {
            candidate[coordinate] = moved;
        }
    }
    candidate
}

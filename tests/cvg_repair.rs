//! `cvg::repair`: a feasible point near a given one, deterministically.
//!
//! The contract is Artemis's (the optimizer that consumes this crate): hand
//! over any point in the declared box, get back one the same feasibility
//! oracle passes, **Euclidean-nearest over box-normalised coordinates**, the
//! same answer every time. The design and the alternatives it rejected are
//! in `docs/todo.md` under *Repair for Artemis*.
//!
//! The geometry cases have closed-form answers under that metric — the foot
//! of the perpendicular on a half-space, the radial point on a disc — and
//! every expected value below was derived first and the test written second.
//! The metric used to be L1, under which the nearest point moves one
//! coordinate wherever one can reach; that was retired with the anchors it
//! ranked, because an optimizer stepping over a wall wants to be put back
//! where it stepped from, not slid along the wall (`tests/regression_fixture.rs`,
//! `repair_lands_axis_aligned_not_nearest`). The answer is a function of the
//! constraints alone, with nothing else pulling on it; the disc's corners pin
//! that.

mod common;

use anyhow::Context;
use faer::Mat;
use rand::RngExt;
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{ConstraintSolver, ConstraintSystem, FeasibleRegion, InputVariable, RepairError};

/// A fixture's [`ConstraintSystem`]; one that does not bind is the test's error.
fn system(variables: Vec<InputVariable>, constraints: &[&str]) -> anyhow::Result<ConstraintSystem> {
    Ok(ConstraintSystem::new(
        variables,
        constraints.iter().copied(),
    )?)
}

/// The region a solve of `system` returns, which is where `repair` lives.
///
/// The census it produces is not used: `repair` is a function of the system,
/// the point and the clearance, so no expected value below depends on the
/// sampler. The solve is the price of a region, and on these fixtures it is
/// milliseconds. An unsatisfiable fixture is an error for the test to
/// propagate, not a verdict for this to pass judgement on.
fn region(system: &ConstraintSystem) -> anyhow::Result<FeasibleRegion> {
    ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))
        .context("the fixture should be satisfiable")
}

fn variables(specs: &[(&str, f64, f64)]) -> Vec<InputVariable> {
    specs
        .iter()
        .map(|(name, low, high)| InputVariable::new(*name, *low, *high))
        .collect()
}

/// Same value the other cvg suites use, so a point seen in one is the point
/// seen in another.
const SEED: u64 = 0x50_50_1E_5E_ED;

/// The clearance the clearance fixtures ask for: a thousandth of each box
/// width, large enough to see in a closed-form answer and small next to every
/// fixture's geometry. The geometry fixtures above it run at `0.0`, which is
/// the landing-on-the-bound contract, pinned as it was.
const CLEARANCE: f64 = 1e-3;

/// Whether every constraint holds at `point`, judged independently of `repair`
/// through the public evaluator. A test that trusts the thing it is testing is
/// not a test. Strict: the residual must be `<= 0`, no tolerance, because that
/// is what the caller's own evaluator will demand.
fn holds(system: &ConstraintSystem, point: &[f64]) -> bool {
    let bindings: Vec<(&str, f64)> = system
        .variables()
        .iter()
        .zip(point)
        .map(|(variable, value)| (variable.name.as_str(), *value))
        .collect();
    system.constraints().all(|constraint| {
        common::eval_one(constraint, &bindings).is_ok_and(|residual| residual <= 0.0)
    })
}

/// Whether `point` and each of its `2d` axis neighbours `clearance` box
/// widths away hold, through the same independent evaluator. This is the
/// clearance contract as `repair` states it, checked without `repair`.
fn has_clearance(system: &ConstraintSystem, point: &[f64], clearance: f64) -> bool {
    if !holds(system, point) || !in_box(system, point) {
        return false;
    }
    let mut neighbour = point.to_vec();
    for (coordinate, variable) in system.variables().iter().enumerate() {
        let step = clearance * (variable.upper_bound - variable.lower_bound);
        for sign in [-1.0, 1.0] {
            neighbour[coordinate] = point[coordinate] + sign * step;
            if !holds(system, &neighbour) || !in_box(system, &neighbour) {
                return false;
            }
        }
        neighbour[coordinate] = point[coordinate];
    }
    true
}

fn in_box(system: &ConstraintSystem, point: &[f64]) -> bool {
    system
        .variables()
        .iter()
        .zip(point)
        .all(|(variable, value)| variable.contains(*value))
}

/// Euclidean distance over box-normalised coordinates, the metric `repair`
/// promises "near" in.
fn normalised_l2(system: &ConstraintSystem, a: &[f64], b: &[f64]) -> f64 {
    system
        .variables()
        .iter()
        .zip(a.iter().zip(b))
        .map(|(variable, (x, y))| {
            let scaled = (x - y) / (variable.upper_bound - variable.lower_bound);
            scaled * scaled
        })
        .sum::<f64>()
        .sqrt()
}

#[test]
fn a_half_space_is_entered_along_its_normal() -> anyhow::Result<()> {
    // `2*x1 + x2 < 1` from (1, 1). The foot of the perpendicular is
    // (0.2, 0.6): both coordinates move, in the ratio of the normal. The
    // clamp alone would move `x1` to 0 and leave `x2` — a cost of 1 against
    // the foot's 0.894 — and the projection from that landing slides it
    // along the wall to the foot.
    let system = system(
        variables(&[("x1", -2.0, 2.0), ("x2", -2.0, 2.0)]),
        &["2*x1 + x2 < 1"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[1.0, 1.0], 0.0)
        .context("a half-space is reachable")?;

    assert!(
        holds(&system, &repaired),
        "{repaired:?} violates the half-space"
    );
    assert!(
        (repaired[0] - 0.2).abs() < 1e-10 && (repaired[1] - 0.6).abs() < 1e-10,
        "the foot of the normal is (0.2, 0.6), got {repaired:?}"
    );
    Ok(())
}

#[test]
fn a_disc_is_entered_radially() -> anyhow::Result<()> {
    // From (2, 0.5) the nearest point of the unit disc is straight toward
    // its centre: (2, 0.5) / sqrt(4.25). The clamp alone lands at
    // (sqrt(0.75), 0.5) — `y` already in range, only `x` moved — which is
    // where the L1 diamond touches the disc, 0.14 farther than the radial
    // point.
    //
    // Written with `sqr`; the next fixture is the same disc spelled `x^2`.
    let system = system(
        variables(&[("x", -2.0, 2.0), ("y", -2.0, 2.0)]),
        &["sqr(x) + sqr(y) < 1"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[2.0, 0.5], 0.0)
        .context("a disc is reachable")?;

    assert!(
        holds(&system, &repaired),
        "{repaired:?} is outside the disc"
    );
    let scale = 4.25_f64.sqrt();
    assert!(
        (repaired[0] - 2.0 / scale).abs() < 1e-10 && (repaired[1] - 0.5 / scale).abs() < 1e-10,
        "the radial point is {:?}, got {repaired:?}",
        [2.0 / scale, 0.5 / scale]
    );
    Ok(())
}

#[test]
fn a_disc_spelled_with_a_power_is_entered_the_same_way() -> anyhow::Result<()> {
    // `x^2` is how every optimizer formulation spells it. Narrowing inverts a
    // whole power through its root, so the clamp's warm start is the one the
    // `sqr` spelling gets, and the projection lands the same radial point.
    let system = system(
        variables(&[("x", -2.0, 2.0), ("y", -2.0, 2.0)]),
        &["x^2 + y^2 < 1"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[2.0, 0.5], 0.0)
        .context("a disc is reachable")?;

    assert!(
        holds(&system, &repaired),
        "{repaired:?} is outside the disc"
    );
    let scale = 4.25_f64.sqrt();
    assert!(
        (repaired[0] - 2.0 / scale).abs() < 1e-10 && (repaired[1] - 0.5 / scale).abs() < 1e-10,
        "the radial point is {:?}, got {repaired:?}",
        [2.0 / scale, 0.5 / scale]
    );
    Ok(())
}

#[test]
fn a_driven_coordinate_is_not_privileged() -> anyhow::Result<()> {
    // `2*x1 + x2 == 3 +/- 0.001` from (3, 3). Driving `x2` to satisfy the
    // equality moves it to -2.999, a cost of 6; clamping `x1` moves it to
    // 0.0005, a cost of 3; the foot of the perpendicular on the slab's near
    // face, (0.6, 1.8) shifted by the band, costs 2.68 and is the answer.
    // The equality classifies as driven, and repair must still land the
    // nearest point rather than the computed one.
    let system = system(
        variables(&[("x1", -5.0, 5.0), ("x2", -5.0, 5.0)]),
        &["2*x1 + x2 == 3 +/- 0.001"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[3.0, 3.0], 0.0)
        .context("a slab is reachable")?;

    assert!(
        holds(&system, &repaired),
        "{repaired:?} is outside the slab"
    );
    // The near face is `2 x1 + x2 = 3.001`; its foot from (3, 3) is
    // (3, 3) - (5.999 / 5) (2, 1).
    let foot = [3.0 - 2.0 * 5.999 / 5.0, 3.0 - 5.999 / 5.0];
    assert!(
        (repaired[0] - foot[0]).abs() < 1e-10 && (repaired[1] - foot[1]).abs() < 1e-10,
        "the foot of the normal on the near face is {foot:?}, got {repaired:?}"
    );
    Ok(())
}

#[test]
fn the_nearer_band_wins() -> anyhow::Result<()> {
    // Two bands, at -2 and 1, each about 0.00033 wide. No interval narrowing
    // separates them, so this is decided by the projection: from 0.9 the band
    // at 1 is a tenth away and the band at -2 is nearly three, and from -1 it
    // is the other way round.
    let system = system(
        variables(&[("x", -5.0, 5.0)]),
        &["(x + 2) * (x - 1) == 0 +/- 0.001"],
    )?;
    let region = region(&system)?;

    let near_one = region.repair(&[0.9], 0.0).context("a band is reachable")?;
    assert!(
        holds(&system, &near_one),
        "{near_one:?} is outside both bands"
    );
    assert!(
        (near_one[0] - 1.0).abs() < 0.001,
        "from 0.9 the band at 1 is nearer, got {}",
        near_one[0]
    );

    let near_minus_two = region.repair(&[-1.0], 0.0).context("a band is reachable")?;
    assert!(
        holds(&system, &near_minus_two),
        "{near_minus_two:?} is outside both bands"
    );
    assert!(
        (near_minus_two[0] + 2.0).abs() < 0.001,
        "from -1 the band at -2 is nearer, got {}",
        near_minus_two[0]
    );
    Ok(())
}

#[test]
fn a_domain_hole_is_just_infeasible() -> anyhow::Result<()> {
    // `ln(x1) > 0` cannot be evaluated at -0.5: the evaluator faults rather than
    // producing a residual. That is an infeasible point like any other, and the
    // interval `ln` inverts to says where the feasible ones are. Where exactly
    // the boundary sits is the evaluator's call — its own rounding admits
    // `x1 = 1` — so the claim is "at the boundary as the oracle draws it", not
    // "above 1 in the reals".
    let system = system(variables(&[("x1", -1.0, 3.0)]), &["ln(x1) > 0"])?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[-0.5], 0.0)
        .context("the log's domain is reachable")?;

    assert!(
        holds(&system, &repaired),
        "{repaired:?} is outside ln's feasible range"
    );
    assert!(
        (repaired[0] - 1.0).abs() < 1e-9,
        "x1 should land on the boundary at 1, got {}",
        repaired[0]
    );
    Ok(())
}

#[test]
fn two_hundred_bounds_are_landed_on_exactly() -> anyhow::Result<()> {
    // Every coordinate bounded below by 10.5, every one starting at 10.2. A
    // clamp lands each on its bound in one sweep, and the landing must be
    // exact: Artemis measured coordinates *at* a bound coming out several times
    // more accurate than ones merely near it.
    const DIMENSIONS: usize = 200;
    let names: Vec<String> = (1..=DIMENSIONS).map(|i| format!("x{i}")).collect();
    let specs: Vec<(&str, f64, f64)> = names.iter().map(|n| (n.as_str(), 10.0, 11.0)).collect();
    let sources: Vec<String> = names.iter().map(|n| format!("{n} > 10.5")).collect();
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let system = system(variables(&specs), &sources)?;
    let region = region(&system)?;

    let repaired = region
        .repair(&vec![10.2; DIMENSIONS], 0.0)
        .context("a corner is reachable")?;

    assert!(
        holds(&system, &repaired),
        "some coordinate is not above its bound"
    );
    for (index, value) in repaired.iter().enumerate() {
        assert!(
            *value > 10.5 && value - 10.5 < 1e-9,
            "x{} should land just above 10.5, got {value}",
            index + 1
        );
    }
    Ok(())
}

#[test]
fn repair_holds_its_contract_over_a_polytope() -> anyhow::Result<()> {
    // Five variables under three loose inequalities, with a census from a
    // solve: the shape Artemis actually runs. For points scattered over the
    // whole box: the result is feasible with clearance by an independent
    // evaluation, inside the box, a fixed point of `repair`, the same on a
    // second call, and never farther than the nearest census point that has
    // the clearance. That last one is a quality bar on the projection rather
    // than a contract — the census is not consulted — and a landing farther
    // than a point the walker happened to find would be a projection that had
    // not found the boundary.
    const CENSUS: usize = 256;
    const TRIALS: usize = 64;
    let inputs = variables(&[
        ("x1", 0.0, 1.0),
        ("x2", 0.0, 1.0),
        ("x3", 0.0, 1.0),
        ("x4", 0.0, 1.0),
        ("x5", 0.0, 1.0),
    ]);
    let sources = ["x1 + x2 > x3", "x2 + x3 > x4", "x3 + x4 > x5"];
    let system = system(inputs.clone(), &sources)?;

    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let census = region.sample(
        Mat::zeros(0, 0).as_ref(),
        CENSUS,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;
    assert_eq!(census.ncols(), CENSUS, "the census should fill");
    // Only a census point with the clearance is one `repair` could have been
    // no worse than. Judged once here: the independent evaluator compiles per
    // call, and this is the hot loop.
    let clear_census: Vec<Vec<f64>> = (0..CENSUS)
        .map(|column| {
            (0..census.nrows())
                .map(|row| census[(row, column)])
                .collect()
        })
        .filter(|point: &Vec<f64>| has_clearance(&system, point, CLEARANCE))
        .collect();
    assert!(
        !clear_census.is_empty(),
        "the census should have room to spare"
    );

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(SEED);
    let mut complaints = Vec::new();
    for _ in 0..TRIALS {
        let point: Vec<f64> = (0..inputs.len())
            .map(|_| rng.random_range(0.0..1.0))
            .collect();
        let Ok(repaired) = region.repair(&point, CLEARANCE) else {
            complaints.push(format!("{point:?}: no repair"));
            continue;
        };
        if !has_clearance(&system, &repaired, CLEARANCE) {
            complaints.push(format!("{point:?} -> {repaired:?}: no clearance"));
        }
        if !holds(&system, &repaired) {
            complaints.push(format!("{point:?} -> {repaired:?}: infeasible"));
        }
        if !in_box(&system, &repaired) {
            complaints.push(format!("{point:?} -> {repaired:?}: outside the box"));
        }
        let again = region.repair(&repaired, CLEARANCE);
        if again.as_deref() != Ok(repaired.as_slice()) {
            complaints.push(format!(
                "{point:?} -> {repaired:?} -> {again:?}: not a fixed point"
            ));
        }
        let twice = region.repair(&point, CLEARANCE);
        let same = twice.as_ref().is_ok_and(|twice| {
            twice
                .iter()
                .zip(&repaired)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        });
        if !same {
            complaints.push(format!(
                "{point:?} -> {repaired:?} then {twice:?}: not deterministic"
            ));
        }
        let moved = normalised_l2(&system, &point, &repaired);
        let nearest = clear_census
            .iter()
            .map(|sample| normalised_l2(&system, &point, sample))
            .fold(f64::INFINITY, f64::min);
        if moved > nearest {
            complaints.push(format!(
                "{point:?} -> {repaired:?}: moved {moved} where a census point was {nearest} away"
            ));
        }
    }
    assert!(complaints.is_empty(), "{}", complaints.join("\n"));
    Ok(())
}

#[test]
fn between_two_bands_the_nearer_is_reached() -> anyhow::Result<()> {
    // Between the two bands, where no interval says which way to go: the
    // chord this replaced needed an anchor to bisect toward and answered
    // `Stranded` without one. The projection needs nothing but the
    // constraint, and from 0 the band at 1 is the nearer.
    let system = system(
        variables(&[("x", -5.0, 5.0)]),
        &["(x + 2) * (x - 1) == 0 +/- 0.001"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[0.0], 0.0)
        .context("a band is reachable from between them")?;
    assert!(
        holds(&system, &repaired),
        "{repaired:?} is outside both bands"
    );
    assert!(
        (repaired[0] - 1.0).abs() < 0.001,
        "from 0 the band at 1 is nearer, got {}",
        repaired[0]
    );
    Ok(())
}

/// The record of what anchors did, and the reason they are gone. With
/// anchors clustered on the rim at angle zero, the corner `(1.2, 1.2)` —
/// where both axis slices are empty and clamping cannot land — used to land
/// at 17°: the chord from the nearest anchor is a convex combination of
/// anchor and proposal, and every coordinate was dragged toward the census.
/// Over thousands of repairs that steered the optimizer toward wherever the
/// census was, rather than along the boundary its objective preferred. The
/// projection lands every corner radially, within a fraction of a degree,
/// and would land it there whatever else the region had ever produced.
#[test]
fn a_corner_of_a_disc_is_entered_radially() -> anyhow::Result<()> {
    const CLEARANCE: f64 = 1e-9;
    let system = system(
        variables(&[("x", -2.0, 2.0), ("y", -2.0, 2.0)]),
        &["x^2 + y^2 < 1"],
    )?;
    let region = region(&system)?;

    let mut complaints = Vec::new();
    for corner in [[1.2, 1.2], [-1.2, 1.2], [-1.2, -1.2], [1.2, -1.2]] {
        let repaired = region
            .repair(&corner, CLEARANCE)
            .with_context(|| format!("the rim is reachable from {corner:?}"))?;
        let angle = repaired[1].atan2(repaired[0]).to_degrees();
        let wanted = corner[1].atan2(corner[0]).to_degrees();
        let radius = repaired[0].hypot(repaired[1]);
        if (angle - wanted).abs() > 0.5 {
            complaints.push(format!(
                "{corner:?} -> {repaired:?}: at {angle:.2} degrees, not {wanted:.0}"
            ));
        }
        if !(1.0 - 1e-3..1.0).contains(&radius) {
            complaints.push(format!(
                "{corner:?} -> {repaired:?}: at radius {radius}, not on the rim"
            ));
        }
        if !has_clearance(&system, &repaired, CLEARANCE) {
            complaints.push(format!("{corner:?} -> {repaired:?}: no clearance"));
        }
    }
    assert!(complaints.is_empty(), "{}", complaints.join("\n"));
    Ok(())
}

#[test]
fn a_bound_is_reached() -> anyhow::Result<()> {
    // The clamp: the constraint itself says where the feasible side is.
    let system = system(
        variables(&[("x1", -2.0, 2.0), ("x2", -2.0, 2.0)]),
        &["2*x1 + x2 < 1"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[1.0, 1.0], 0.0)
        .context("a half-space is entered")?;
    assert!(
        holds(&system, &repaired),
        "{repaired:?} violates the half-space"
    );
    Ok(())
}

#[test]
fn a_feasible_point_is_returned_untouched() -> anyhow::Result<()> {
    let system = system(
        variables(&[("x1", -2.0, 2.0), ("x2", -2.0, 2.0)]),
        &["2*x1 + x2 < 1"],
    )?;
    let region = region(&system)?;
    let point = [-0.3, 0.7];

    assert_eq!(region.repair(&point, 0.0).as_deref(), Ok(point.as_slice()));
    Ok(())
}

// ---- clearance: a deliberate step inside, not an ulp ----------------------

#[test]
fn a_half_space_is_entered_clear_of_its_wall() -> anyhow::Result<()> {
    // The normal fixture again, asked for a thousandth of the box. The
    // landing is the foot (0.2, 0.6) stepped inside so that every axis
    // neighbour `CLEARANCE * 4` away holds: at least that far from the wall
    // along the coordinate the wall is steepest in.
    let system = system(
        variables(&[("x1", -2.0, 2.0), ("x2", -2.0, 2.0)]),
        &["2*x1 + x2 < 1"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[1.0, 1.0], CLEARANCE)
        .context("a half-space is reachable")?;

    assert!(
        has_clearance(&system, &repaired, CLEARANCE),
        "{repaired:?} lacks the clearance"
    );
    let off_the_foot = ((repaired[0] - 0.2).powi(2) + (repaired[1] - 0.6).powi(2)).sqrt();
    assert!(
        off_the_foot < 4.0 * CLEARANCE * 4.0,
        "should land within a few clearances of the foot (0.2, 0.6), got {repaired:?}"
    );
    Ok(())
}

#[test]
fn a_vertex_is_landed_clear_of_both_walls() -> anyhow::Result<()> {
    // Two walls meeting at (0.5, 0.5), approached from (1, 1). Each clamp is
    // its own axis projection, so the corner is the closed form: both
    // coordinates land the clearance inside their wall. This is the shape the
    // spring's optimum has, where a landing an ulp inside was found wanting.
    let system = system(
        variables(&[("x1", 0.0, 1.0), ("x2", 0.0, 1.0)]),
        &["x1 < 0.5", "x2 < 0.5"],
    )?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[1.0, 1.0], CLEARANCE)
        .context("a corner is reachable")?;

    assert!(
        has_clearance(&system, &repaired, CLEARANCE),
        "{repaired:?} lacks the clearance"
    );
    for (index, value) in repaired.iter().enumerate() {
        let expected = 0.5 - CLEARANCE;
        assert!(
            (value - expected).abs() < 1e-12,
            "x{} should land at {expected}, got {value}",
            index + 1
        );
    }
    Ok(())
}

#[test]
fn a_chord_landing_is_backed_off() -> anyhow::Result<()> {
    // The two-bands fixture, which only the shotgun can answer, asked for a
    // clearance a tenth of a band's half-width: the chord lands on the band's
    // edge and the answer must be stepped inside it.
    let system = system(
        variables(&[("x", -5.0, 5.0)]),
        &["(x + 2) * (x - 1) == 0 +/- 0.001"],
    )?;
    let region = region(&system)?;
    // The band at 1 is about 0.00033 wide in `x`; a hundredth of that, over
    // the box's width of 10.
    let clearance = 0.000_033 / 10.0;

    let repaired = region
        .repair(&[0.9], clearance)
        .context("a band is reachable")?;

    assert!(
        has_clearance(&system, &repaired, clearance),
        "{repaired:?} lacks the clearance"
    );
    assert!(
        (repaired[0] - 1.0).abs() < 0.001,
        "from 0.9 the band at 1 is nearer, got {}",
        repaired[0]
    );
    Ok(())
}

#[test]
fn a_slab_thinner_than_the_clearance_is_cramped() -> anyhow::Result<()> {
    // A slab `0.002` wide in a box `2` wide, asked for a clearance of a
    // hundredth of the box: `0.02` each side, ten times more room than the
    // slab has. Nothing can be handed back with that clearance, and the
    // honest answer names the nearest feasible point and says so.
    let system = system(variables(&[("x", -1.0, 1.0)]), &["x == 0 +/- 0.001"])?;
    let region = region(&system)?;

    let verdict = region.repair(&[0.5], 1e-2);

    match verdict {
        Err(RepairError::Cramped { nearest, clearance }) => {
            assert!(holds(&system, &nearest), "{nearest:?} is not even feasible");
            assert_eq!(clearance, 1e-2);
            assert!(
                nearest[0].abs() <= 0.001,
                "the nearest feasible point should be in the slab, got {}",
                nearest[0]
            );
        }
        other => panic!("expected Cramped, got {other:?}"),
    }
    Ok(())
}

#[test]
fn a_point_with_clearance_is_returned_untouched() -> anyhow::Result<()> {
    let system = system(
        variables(&[("x1", -2.0, 2.0), ("x2", -2.0, 2.0)]),
        &["2*x1 + x2 < 1"],
    )?;
    let region = region(&system)?;
    let point = [-0.3, 0.7];

    assert_eq!(
        region.repair(&point, CLEARANCE).as_deref(),
        Ok(point.as_slice())
    );
    Ok(())
}

#[test]
fn a_feasible_point_without_clearance_is_moved_inward() -> anyhow::Result<()> {
    // Feasible by a hair — `2 * x1 + x2` is `1 - 1e-12` — is the fixed point
    // the caller sent back in, and with a clearance it is not returned as it
    // came: it steps inside the wall. Not necessarily along one axis: on a
    // wall every coordinate's clamp costs exactly the clearance, the tie goes
    // to rounding, and a coordinate that moved first without clearing the
    // other's neighbour is not always put back. What is promised is the
    // clearance, and a move of at most the clearance per coordinate.
    let system = system(
        variables(&[("x1", -2.0, 2.0), ("x2", -2.0, 2.0)]),
        &["2*x1 + x2 < 1"],
    )?;
    let region = region(&system)?;
    let point = [0.0, 1.0 - 1e-12];
    assert!(holds(&system, &point), "the fixture should start feasible");
    assert!(!has_clearance(&system, &point, CLEARANCE));

    let repaired = region
        .repair(&point, CLEARANCE)
        .context("a half-space is entered")?;

    assert!(
        has_clearance(&system, &repaired, CLEARANCE),
        "{repaired:?} lacks the clearance"
    );
    let moved = normalised_l2(&system, &point, &repaired);
    assert!(
        moved <= 2.0 * CLEARANCE + 1e-12,
        "{point:?} -> {repaired:?} moved {moved}, more than the clearance per coordinate"
    );
    for (index, (before, after)) in point.iter().zip(&repaired).enumerate() {
        assert!(
            after <= before,
            "x{} should only ever step inward, {before} -> {after}",
            index + 1
        );
    }
    Ok(())
}

#[test]
fn a_box_bound_is_a_wall_too() -> anyhow::Result<()> {
    // A constraint that never binds; the box's own edge is the only wall. A
    // point on it has no room to move outward, so it is moved the clearance
    // inside — the caller's round trip can miss the edge by an ulp as well.
    let system = system(variables(&[("x", 0.0, 1.0)]), &["x > -1"])?;
    let region = region(&system)?;

    let repaired = region
        .repair(&[1.0], CLEARANCE)
        .context("the box's inside is reachable")?;

    assert!(
        has_clearance(&system, &repaired, CLEARANCE),
        "{repaired:?} lacks the clearance"
    );
    assert!(
        (repaired[0] - (1.0 - CLEARANCE)).abs() < 1e-12,
        "x should step to {}, got {}",
        1.0 - CLEARANCE,
        repaired[0]
    );
    Ok(())
}

// ---- the ball oracle: the local projection is the projection ---------------

/// The clearance the ball-oracle tests repair with.
const BALL_CLEARANCE: f64 = 1e-9;

/// A box and the constraints over it.
type Shape<'a> = (&'a [(&'a str, f64, f64)], &'a [&'a str]);

/// A brute-force second opinion on "nearest": a feasible point with the
/// clearance, drawn uniformly from the ball of the repair's own radius around
/// the proposal, that is nearer than the repair — `None` when twenty thousand
/// draws found none. Low dimension only: uniform points in a ball localise
/// nothing past a handful of variables, which is why this is an oracle in a
/// test and not a stage in `repair`.
///
/// Candidates are judged by the system's own oracle: this is a question about
/// the geometry, not about the evaluator, and the independent one compiles per
/// call.
fn nearer_in_the_ball(
    system: &ConstraintSystem,
    proposal: &[f64],
    repaired: &[f64],
    allowance: f64,
    rng: &mut Xoshiro256PlusPlus,
) -> Option<(Vec<f64>, f64)> {
    const DRAWS: usize = 20_000;
    let widths: Vec<f64> = system
        .variables()
        .iter()
        .map(|variable| variable.upper_bound - variable.lower_bound)
        .collect();
    let radius = normalised_l2(system, proposal, repaired);

    // Uniform in the ball: draws in its bounding cube, kept if inside.
    let mut nearest = radius;
    let mut culprit = None;
    for _ in 0..DRAWS {
        let candidate: Vec<f64> = proposal
            .iter()
            .zip(&widths)
            .map(|(centre, width)| centre + rng.random_range(-radius..=radius) * width)
            .collect();
        let at = normalised_l2(system, proposal, &candidate);
        if at < nearest * (1.0 - allowance) && system.is_feasible(&candidate, BALL_CLEARANCE) {
            nearest = at;
            culprit = Some(candidate);
        }
    }
    culprit.map(|culprit| (culprit, nearest))
}

/// The closed-form fixtures above pin particular geometries; this pins the
/// claim on smooth shapes that have no closed form — a ring, a sine band, a
/// cubic — where the Newton or COBYLA landing could in principle be a local
/// projection and not the projection.
#[test]
fn no_point_in_the_repairs_own_ball_is_nearer() -> anyhow::Result<()> {
    const PROPOSALS: usize = 4;
    /// The projection converges to `1e-13` of the box, the clamp lands a
    /// clearance inside; a hit nearer by less than this is the same point.
    const ALLOWANCE: f64 = 1e-6;
    let shapes: &[Shape<'_>] = &[
        (&[("x", -2.0, 2.0), ("y", -2.0, 2.0)], &["x^2 + y^2 < 1"]),
        (
            &[("x", -2.0, 2.0), ("y", -2.0, 2.0)],
            &["x^2 + y^2 < 1.5", "x^2 + y^2 > 0.5"],
        ),
        (
            &[("x", -3.0, 3.0), ("y", -3.0, 3.0)],
            &["y < sin(x) + 0.2", "y > sin(x) - 0.2"],
        ),
        (&[("x", -2.0, 2.0), ("y", -2.0, 2.0)], &["y > x^3 - x"]),
        (
            &[("x", -2.0, 2.0), ("y", -2.0, 2.0), ("z", -2.0, 2.0)],
            &["x^2 + y^2 + z^2 < 1", "x + y + z > 0.3"],
        ),
        (
            &[("x1", -5.0, 5.0), ("x2", -5.0, 5.0), ("x3", -5.0, 5.0)],
            &["x1 == x2 + x3 +/- 0.01", "x1 * x2 < 1"],
        ),
    ];

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(SEED);
    let mut complaints = Vec::new();
    for (specs, sources) in shapes {
        let system = system(variables(specs), sources)?;
        let region = region(&system)?;
        for _ in 0..PROPOSALS {
            let proposal: Vec<f64> = specs
                .iter()
                .map(|(_, lo, hi)| rng.random_range(*lo..*hi))
                .collect();
            if system.is_feasible(&proposal, BALL_CLEARANCE) {
                continue;
            }
            let repaired = match region.repair(&proposal, BALL_CLEARANCE) {
                Ok(repaired) => repaired,
                Err(error) => {
                    complaints.push(format!("{sources:?} from {proposal:?}: {error}"));
                    continue;
                }
            };
            if let Some((culprit, at)) =
                nearer_in_the_ball(&system, &proposal, &repaired, ALLOWANCE, &mut rng)
            {
                complaints.push(format!(
                    "{sources:?} from {proposal:?}: repaired to {repaired:?} at {:.6}, but \
                     {culprit:?} is feasible at {at:.6}",
                    normalised_l2(&system, &proposal, &repaired)
                ));
            }
        }
    }
    assert!(complaints.is_empty(), "{}", complaints.join("\n"));
    Ok(())
}

/// The same claim on shapes with a *jump* in them — `floor`, `%` — where no
/// slice reads a wall, no gradient exists, and COBYLA's linear models of a
/// staircase say nothing. Every stage of `repair` declines, and before the
/// sampling box the answer came from the reference path alone: a chord from
/// a far-off feasible point, landing *somewhere* feasible — 0.40 of the box
/// away on the comb where a cell sat at 0.20, 0.235 on the checkerboard
/// where one sat at 0.007. The region is fat enough that a few thousand
/// draws around the proposal find the nearest cell, which is what the box
/// is for. The allowance is a sampling method's: a few percent, not a
/// projection's `1e-6`.
#[test]
fn no_point_in_the_repairs_own_ball_is_nearer_across_a_jump() -> anyhow::Result<()> {
    const ALLOWANCE: f64 = 0.05;
    let shapes: &[(Shape<'_>, &[f64])] = &[
        (
            (
                &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
                &["floor(x1 * 100) % 7 == 0 +/- 0.1", "x2 < 3"],
            ),
            &[5.003, 5.0],
        ),
        (
            (
                &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
                &[
                    "(floor(x1 * 10) + floor(x2 * 10)) % 5 == 0 +/- 0.1",
                    "floor(x1 * x2) % 3 == 0 +/- 0.1",
                ],
            ),
            &[5.25, 5.25],
        ),
        (
            (
                &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0), ("x3", 0.0, 10.0)],
                &[
                    "floor(x1) == 3 +/- 0.1",
                    "floor(x2) == 6 +/- 0.1",
                    "floor(x3) == 1 +/- 0.1",
                ],
            ),
            &[7.5, 1.5, 8.0],
        ),
        // A jump beside a driven equality: the band is a millionth wide, so
        // it is never sampled, only computed — every draw's `x2` put on the
        // curve — and the box still lands the nearest cell.
        (
            (
                &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
                &[
                    "floor(x1 * 100) % 7 == 0 +/- 0.1",
                    "x2 == sin(x1) + 1 +/- 0.000001",
                ],
            ),
            &[5.003, 5.0],
        ),
        // The same beside a driven product, where COBYLA's landing sat ten
        // units away: its evaluations fell feasible far from the point.
        (
            (
                &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0), ("x3", 0.0, 10.0)],
                &[
                    "(floor(x1 * 10) + floor(x2 * 10)) % 5 == 0 +/- 0.1",
                    "x3 == x1 * x2 +/- 0.0001",
                ],
            ),
            &[5.25, 5.25, 9.0],
        ),
    ];

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(SEED);
    let mut complaints = Vec::new();
    for ((specs, sources), proposal) in shapes {
        let system = system(variables(specs), sources)?;
        let region = region(&system)?;
        let repaired = match region.repair(proposal, BALL_CLEARANCE) {
            Ok(repaired) => repaired,
            Err(error) => {
                complaints.push(format!("{sources:?} from {proposal:?}: {error}"));
                continue;
            }
        };
        if let Some((culprit, at)) =
            nearer_in_the_ball(&system, proposal, &repaired, ALLOWANCE, &mut rng)
        {
            complaints.push(format!(
                "{sources:?} from {proposal:?}: repaired to {repaired:?} at {:.6}, but \
                 {culprit:?} is feasible at {at:.6}",
                normalised_l2(&system, proposal, &repaired)
            ));
        }
    }
    assert!(complaints.is_empty(), "{}", complaints.join("\n"));
    Ok(())
}

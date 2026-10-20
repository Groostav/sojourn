//! Discrete variables, and systems that mix them with continuous ones: the
//! spec the engine is driven against.
//!
//! A discrete variable takes a lattice (`lower, step, upper`) or a list of
//! levels, and nothing between. What that asks of each rung:
//!
//! * **The oracle** judges a point off a level as outside the box, and a
//!   clearance does not apply to a coordinate that cannot move a little.
//! * **Proof** gets stronger: an interval rounded inward onto levels can be
//!   empty where the interval was not, and a box narrowed to one level in
//!   every coordinate is a point that can be judged. When the levels are what
//!   miss the region — the constraints can be met continuously — the verdict
//!   says so, and hands back the continuous witness.
//! * **A design** over a system of discrete variables alone is over a finite
//!   set, and asking for more points than it holds is answered with all of
//!   them and a proof that there are no more.
//! * **Driving** computes a continuous coordinate from discrete ones and never
//!   the reverse.
//! * **Repair** is nearest in box-normalised Euclidean distance, levels
//!   included: it changes a level when that is nearer than staying.
//!
//! Every oracle here is independent of the engine: feasibility is re-checked
//! by evaluating each constraint, and a level by comparing with the values
//! declared.

mod common;

use faer::Mat;

use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{
    ConstraintSolver, ConstraintSystem, Domain, FeasibleRegion, Infeasibility, InputVariable,
    SampleError,
};

/// A fixture's [`ConstraintSystem`]; one that does not bind is the test's error.
fn system(variables: Vec<InputVariable>, constraints: &[&str]) -> anyhow::Result<ConstraintSystem> {
    Ok(ConstraintSystem::new(
        variables,
        constraints.iter().copied(),
    )?)
}

/// Pinned so a failure is reproducible.
const SEED: u64 = 0x50_50_1E_5E_ED;

fn seeded() -> Xoshiro256PlusPlus {
    Xoshiro256PlusPlus::seed_from_u64(SEED)
}

fn solver() -> ConstraintSolver {
    ConstraintSolver::new().with_proposal_budget(common::PROPOSAL_BUDGET)
}

/// The region of a fixture that is satisfiable by construction.
fn region(system: &ConstraintSystem) -> anyhow::Result<FeasibleRegion> {
    Ok(solver().solve(system, &mut seeded())?)
}

/// A sample matrix back as one `Vec<f64>` per point.
fn columns(samples: &Mat<f64>) -> Vec<Vec<f64>> {
    (0..samples.ncols())
        .map(|column| {
            (0..samples.nrows())
                .map(|row| samples[(row, column)])
                .collect()
        })
        .collect()
}

/// Points in a canonical order, so two sets of them compare with `==`.
fn sorted(mut points: Vec<Vec<f64>>) -> Vec<Vec<f64>> {
    points.sort_by(|a, b| {
        a.iter()
            .zip(b)
            .map(|(x, y)| x.total_cmp(y))
            .find(|ordering| ordering.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    points
}

/// Whether `value` is one `domain` declares, judged without the engine: a
/// listed level exactly, a lattice level to a millionth of a step (the exact
/// decimal rule is pinned separately, by the decimal fixture).
fn declared(domain: &Domain, value: f64) -> bool {
    match domain {
        Domain::Continuous { lower, upper } => (*lower..=*upper).contains(&value),
        Domain::Stepped { lower, step, upper } => {
            let k = ((value - lower) / step).round();
            (*lower..=*upper).contains(&value) && (value - (lower + k * step)).abs() <= step * 1e-6
        }
        Domain::Levels(levels) => levels.contains(&value),
    }
}

/// Every point is on its variables' declared values and satisfies every
/// source, re-checked by evaluating each constraint on its own.
fn assert_admissible(system: &ConstraintSystem, sources: &[&str], points: &[Vec<f64>]) {
    for point in points {
        for (variable, value) in system.variables().iter().zip(point) {
            assert!(
                declared(&variable.domain, *value),
                "{} = {value} is not a value {:?} declares",
                variable.name,
                variable.domain
            );
        }
        let bindings: Vec<(&str, f64)> = system
            .variables()
            .iter()
            .map(|variable| variable.name.as_str())
            .zip(point.iter().copied())
            .collect();
        for source in sources {
            let residual = common::eval_one(source, &bindings)
                .unwrap_or_else(|e| panic!("evaluating {source:?} at {point:?}: {e}"));
            assert!(
                residual <= 0.0,
                "{point:?} fails {source:?} (residual {residual})"
            );
        }
    }
}

/// The blamed constraints' sources, sorted.
fn sources(blamed: &[sojourn::ConstraintRef]) -> Vec<&str> {
    let mut sources: Vec<&str> = blamed.iter().map(|c| c.source.as_str()).collect();
    sources.sort_unstable();
    sources
}

// --- The oracle -------------------------------------------------------------

/// A value between two levels is outside the domain the way a value past a
/// bound is outside the box: not feasible, and not a residual either.
#[test]
fn an_off_level_point_is_neither_feasible_nor_graded() -> anyhow::Result<()> {
    let system = system(
        vec![
            InputVariable::new_discrete("n", 0.0, 1.0, 10.0),
            InputVariable::new_continuous("x", 0.0, 1.0),
        ],
        &["n + x < 20"],
    )?;

    let off = [2.5, 0.5];
    let on = [2.0, 0.5];

    assert!(!system.is_feasible(&off, 0.0), "2.5 is not an integer");
    assert_eq!(system.worst_residual(&off), None);
    assert!(system.is_feasible(&on, 0.0));
    assert!(system.worst_residual(&on).is_some());
    Ok(())
}

/// A clearance is room to be perturbed in, and a level is not perturbed: the
/// caller snaps it. So a discrete coordinate on its lowest level has all the
/// clearance asked for, where a continuous one on its wall has none.
#[test]
fn clearance_does_not_apply_to_a_discrete_coordinate() -> anyhow::Result<()> {
    let system = system(
        vec![
            InputVariable::new_discrete("n", 0.0, 1.0, 10.0),
            InputVariable::new_continuous("x", 0.0, 1.0),
        ],
        &["n + x < 20"],
    )?;

    assert!(
        system.is_feasible(&[0.0, 0.5], 0.1),
        "the lowest level is a level, not a wall"
    );
    assert!(
        !system.is_feasible(&[5.0, 0.0], 0.1),
        "the continuous coordinate on its wall still has no clearance"
    );
    Ok(())
}

// --- Proofs -----------------------------------------------------------------

/// The smallest proof over levels: `(2.5, 2.9)` holds no integer. Rounding the
/// contracted interval inward empties it, and since the constraints are met
/// continuously the verdict is that the levels miss, not that the formulation
/// conflicts — with the continuous witness to show it.
#[test]
fn a_value_between_two_levels_is_between_levels() -> anyhow::Result<()> {
    let system = system(
        vec![InputVariable::new_discrete("n", 0.0, 1.0, 10.0)],
        &["n > 2.5", "n < 2.9"],
    )?;

    let verdict = solver().solve(&system, &mut seeded());

    let Err(Infeasibility::BetweenLevels { blamed, relaxed }) = verdict else {
        panic!("expected BetweenLevels, got {verdict:?}");
    };
    assert_eq!(sources(&blamed), vec!["n < 2.9", "n > 2.5"]);
    assert!(
        (2.5..=2.9).contains(&relaxed[0]),
        "the continuous witness is between the bounds: {relaxed:?}"
    );
    Ok(())
}

/// `2x - 2y` is even and `1` is odd: no pair of integers meets this, and any
/// pair of reals on the line `x = y + 0.5` does. Rounding inward climbs one
/// level a visit until a coordinate empties, so contraction proves it alone.
#[test]
fn an_odd_difference_of_evens_is_between_levels() -> anyhow::Result<()> {
    let source = "2*x == 2*y + 1 +/- 0.25";
    let system = system(
        vec![
            InputVariable::new_discrete("x", 0.0, 1.0, 20.0),
            InputVariable::new_discrete("y", 0.0, 1.0, 20.0),
        ],
        &[source],
    )?;

    let verdict = solver().solve(&system, &mut seeded());

    let Err(Infeasibility::BetweenLevels { blamed, relaxed }) = verdict else {
        panic!("expected BetweenLevels, got {verdict:?}");
    };
    assert_eq!(sources(&blamed), vec![source]);
    let residual = common::eval_one(source, &[("x", relaxed[0]), ("y", relaxed[1])])?;
    assert!(
        residual <= 0.0,
        "the witness meets it continuously: {relaxed:?}"
    );
    Ok(())
}

/// `%` narrows nothing, so no interval says an even level is never odd; only
/// judging every level does. Bisection splits the lattice down to single
/// levels, each one a point that fails, and a box of points that all fail is
/// empty — a proof by enumeration, inside the default contraction budget.
#[test]
fn an_odd_level_among_even_steps_is_proved_by_enumeration() -> anyhow::Result<()> {
    let source = "n % 2 == 1 +/- 0.1";
    let system = system(
        vec![InputVariable::new_discrete("n", 0.0, 2.0, 20.0)],
        &[source],
    )?;

    let verdict = solver().solve(&system, &mut seeded());

    let Err(Infeasibility::BetweenLevels { blamed, relaxed }) = verdict else {
        panic!("expected BetweenLevels, got {verdict:?}");
    };
    assert_eq!(sources(&blamed), vec![source]);
    let residual = common::eval_one(source, &[("n", relaxed[0])])?;
    assert!(residual <= 0.0, "the witness is odd: {relaxed:?}");
    Ok(())
}

/// The same parity contradiction on a million levels a side: rounding climbs
/// a millionth of the range a visit, which is no progress, and enumeration
/// is out of budget. Nothing is proved, and nothing is claimed — the twin of
/// the thin and algebraic contradictions in `cvg_pools.rs`.
#[test]
fn an_odd_difference_of_evens_on_a_vast_lattice_is_not_found() -> anyhow::Result<()> {
    let system = system(
        vec![
            InputVariable::new_discrete("x", 0.0, 1.0, 1e6),
            InputVariable::new_discrete("y", 0.0, 1.0, 1e6),
        ],
        &["2*x == 2*y + 1 +/- 0.25"],
    )?;

    let verdict = solver().solve(&system, &mut seeded());

    assert!(
        matches!(verdict, Err(Infeasibility::NotFound { .. })),
        "expected NotFound, got {verdict:?}"
    );
    Ok(())
}

// --- A decimal lattice ------------------------------------------------------

/// `0.1 * 3` is `0.30000000000000004`. A lattice declared in tenths holds the
/// literal `0.3`, or `t <= 0.3` would fail on a value the caller declared and
/// this system would have no point at all. It has exactly one, and a design
/// of two is answered with that one and a proof there is no other.
#[test]
fn a_decimal_step_lands_on_the_literal() -> anyhow::Result<()> {
    let system = system(
        vec![InputVariable::new_discrete("t", 0.0, 0.1, 1.0)],
        &["t >= 0.3", "t <= 0.3"],
    )?;

    let region = region(&system)?;
    let verdict = region.sample(Mat::zeros(0, 0).as_ref(), 2, &mut seeded());

    assert_eq!(region.witness()[0].to_bits(), 0.3_f64.to_bits());
    let Err(SampleError::Exhausted { found, wanted }) = verdict else {
        panic!("expected Exhausted, got {verdict:?}");
    };
    assert_eq!(wanted, 2);
    assert_eq!(columns(&found), vec![vec![0.3]]);
    Ok(())
}

// --- A divider from E12 resistors -------------------------------------------

/// The E12 series from 1k to 100k: twelve values a decade, two decades, and
/// the top. Whole ohms, so every level is exact.
fn e12() -> Vec<f64> {
    let mantissas = [10, 12, 15, 18, 22, 27, 33, 39, 47, 56, 68, 82];
    let mut ohms: Vec<f64> = [100, 1000]
        .iter()
        .flat_map(|decade| mantissas.iter().map(move |m| f64::from(m * decade)))
        .collect();
    ohms.push(100_000.0);
    ohms
}

/// A voltage divider from 5 V to 3.3 V, within a tenth of a volt, from stock
/// resistors: a system of discrete variables alone, sparse in its lattice,
/// and with nothing to drive. Its feasible set is finite and listed by hand
/// here — every pair judged — so the design can be held to it exactly: asked
/// for all of them it returns all of them, and asked for one more it returns
/// the same set with a proof that there is no other.
#[test]
fn a_divider_from_e12_resistors_designs_every_pair_and_no_more() -> anyhow::Result<()> {
    let source = "5 * r2 / (r1 + r2) == 3.3 +/- 0.1";
    let system = system(
        vec![
            InputVariable::new("r1", Domain::Levels(e12())),
            InputVariable::new("r2", Domain::Levels(e12())),
        ],
        &[source],
    )?;
    let mut pairs: Vec<Vec<f64>> = Vec::new();
    for r1 in e12() {
        for r2 in e12() {
            if common::eval_one(source, &[("r1", r1), ("r2", r2)])? <= 0.0 {
                pairs.push(vec![r1, r2]);
            }
        }
    }
    let pairs = sorted(pairs);
    assert_eq!(pairs.len(), 17, "the fixture's own count: {pairs:?}");

    let region = region(&system)?;
    let every = region.sample(Mat::zeros(0, 0).as_ref(), pairs.len(), &mut seeded())?;
    let more = region.sample(Mat::zeros(0, 0).as_ref(), pairs.len() + 1, &mut seeded());

    assert_eq!(sorted(columns(&every)), pairs);
    let Err(SampleError::Exhausted { found, wanted }) = more else {
        panic!("expected Exhausted, got {more:?}");
    };
    assert_eq!(wanted, pairs.len() + 1);
    assert_eq!(sorted(columns(&found)), pairs);
    Ok(())
}

// --- A gearbox: one piece of the region per level ---------------------------

fn gearbox(band: f64) -> (Vec<InputVariable>, Vec<String>) {
    (
        vec![
            InputVariable::new("ratio", Domain::Levels(vec![1.0, 2.0, 4.0])),
            InputVariable::new_continuous("speed", 0.0, 100.0),
        ],
        vec![
            "speed / ratio >= 20".to_owned(),
            format!("speed / ratio <= {}", 20.0 + band),
        ],
    )
}

/// Each ratio has its own stretch of speed — `[20, 25]`, `[40, 50]`,
/// `[80, 100]` — and no two overlap, so nothing moving the ratio with the
/// speed held still ever leaves the ratio it started on. Every ratio still
/// appears in a design, though the first has a quarter of the last's room.
#[test]
fn a_gearbox_gives_every_ratio_a_point() -> anyhow::Result<()> {
    let (variables, sources) = gearbox(5.0);
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let system = system(variables, &sources)?;

    let design = region(&system)?.sample(Mat::zeros(0, 0).as_ref(), 12, &mut seeded())?;

    let points = columns(&design);
    assert_eq!(points.len(), 12);
    assert_admissible(&system, &sources, &points);
    for ratio in [1.0, 2.0, 4.0] {
        assert!(
            points.iter().any(|point| point[0] == ratio),
            "no point at ratio {ratio}: {points:?}"
        );
    }
    Ok(())
}

/// The same gearbox with a band a hundredth wide: a few in ten thousand of
/// the box, below what the probe hands to sampling, so the walker carries
/// the design and must be started on every ratio — the pieces bisection
/// finds.
#[test]
fn a_thin_gearbox_seeds_every_ratio() -> anyhow::Result<()> {
    let (variables, sources) = gearbox(0.01);
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let system = system(variables, &sources)?;

    let design = region(&system)?.sample(Mat::zeros(0, 0).as_ref(), 12, &mut seeded())?;

    let points = columns(&design);
    assert_eq!(points.len(), 12);
    assert_admissible(&system, &sources, &points);
    for ratio in [1.0, 2.0, 4.0] {
        assert!(
            points.iter().any(|point| point[0] == ratio),
            "no point at ratio {ratio}: {points:?}"
        );
    }
    Ok(())
}

// --- Driving across the seam ------------------------------------------------

/// A bolted joint: the load is computed from a count of bolts and a stock
/// diameter, within a newton. The continuous load is the one driven, so the
/// design moves across the discrete choices — many pairs, not the one a
/// chain started on — and draws the load across its band rather than onto
/// the centre line.
#[test]
fn a_load_driven_by_discrete_bolts_spreads_across_its_band() -> anyhow::Result<()> {
    let source = "load == 0.6 * n * d^2 +/- 1";
    let system = system(
        vec![
            InputVariable::new_discrete("n", 3.0, 1.0, 12.0),
            InputVariable::new("d", Domain::Levels(vec![6.0, 8.0, 10.0, 12.0, 16.0, 20.0])),
            InputVariable::new_continuous("load", 0.0, 3000.0),
        ],
        &[source],
    )?;

    let design = region(&system)?.sample(Mat::zeros(0, 0).as_ref(), 30, &mut seeded())?;

    let points = columns(&design);
    assert_eq!(points.len(), 30);
    assert_admissible(&system, &[source], &points);
    let mut pairs: Vec<(f64, f64)> = points.iter().map(|p| (p[0], p[1])).collect();
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pairs.dedup();
    assert!(
        pairs.len() >= 6,
        "only {} (n, d) pairs: {pairs:?}",
        pairs.len()
    );
    assert!(
        points
            .iter()
            .any(|p| (p[2] - 0.6 * p[0] * p[1] * p[1]).abs() > 0.5),
        "every load is on the centre line: {points:?}"
    );
    Ok(())
}

/// `n` occurs once and `length` occurs once, so either could be solved for;
/// only `length` may be. Driving `n` would compute a value between levels.
/// The design holds `n` on its lattice and still moves it.
#[test]
fn an_equality_drives_the_continuous_side_never_the_discrete() -> anyhow::Result<()> {
    let source = "n == length / pitch +/- 0.01";
    let system = system(
        vec![
            InputVariable::new_discrete("n", 1.0, 1.0, 20.0),
            InputVariable::new_continuous("length", 0.0, 100.0),
            InputVariable::new_continuous("pitch", 1.0, 5.0),
        ],
        &[source],
    )?;

    let design = region(&system)?.sample(Mat::zeros(0, 0).as_ref(), 20, &mut seeded())?;

    let points = columns(&design);
    assert_eq!(points.len(), 20);
    assert_admissible(&system, &[source], &points);
    let mut counts: Vec<f64> = points.iter().map(|p| p[0]).collect();
    counts.sort_by(f64::total_cmp);
    counts.dedup();
    assert!(counts.len() > 1, "n never moved: {counts:?}");
    Ok(())
}

/// An equality with nothing it may drive is still a constraint: the lattice
/// is searched, and every point found has `n == 3m`.
#[test]
fn an_equality_between_discrete_variables_still_solves() -> anyhow::Result<()> {
    let source = "n == 3 * m +/- 0.1";
    let system = system(
        vec![
            InputVariable::new_discrete("n", 0.0, 1.0, 30.0),
            InputVariable::new_discrete("m", 0.0, 1.0, 10.0),
        ],
        &[source],
    )?;

    let design = region(&system)?.sample(Mat::zeros(0, 0).as_ref(), 5, &mut seeded())?;

    let points = columns(&design);
    assert_eq!(points.len(), 5);
    assert_admissible(&system, &[source], &points);
    for point in &points {
        assert_eq!(point[0], 3.0 * point[1], "{point:?}");
    }
    Ok(())
}

// --- Repair: nearest, levels included ---------------------------------------

/// Distances are box-normalised: a ratio step of 1 is a third of its range,
/// a speed step of 10 a tenth of its own.
fn gearbox_region() -> anyhow::Result<(ConstraintSystem, FeasibleRegion)> {
    let (variables, sources) = gearbox(5.0);
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let system = system(variables, &sources)?;
    let region = region(&system)?;
    Ok((system, region))
}

fn assert_lands(repaired: &[f64], expected: [f64; 2]) {
    assert_eq!(repaired[0], expected[0], "ratio: {repaired:?}");
    assert!(
        (repaired[1] - expected[1]).abs() < 1e-10,
        "speed: expected {expected:?}, got {repaired:?}"
    );
}

/// From speed 30 at ratio 1, the nearest feasible point keeps the ratio and
/// slows to 25: a twentieth of the speed range, where any other ratio is a
/// third of its range away before speed is counted.
#[test]
fn a_repair_keeps_its_level_when_that_is_nearer() -> anyhow::Result<()> {
    let (_, region) = gearbox_region()?;

    let repaired = region.repair(&[1.0, 30.0], 0.0)?;

    assert_lands(&repaired, [1.0, 25.0]);
    Ok(())
}

/// From speed 90 at ratio 1, keeping the ratio means slowing to 25 — 0.65 of
/// the speed range — while ratio 2 at speed 50 is `hypot(1/3, 0.4)`, about
/// 0.52, and ratio 4 at speed 90 a whole range of ratio away. Pure distance
/// changes the level.
#[test]
fn a_repair_changes_level_when_that_is_nearer() -> anyhow::Result<()> {
    let (_, region) = gearbox_region()?;

    let repaired = region.repair(&[1.0, 90.0], 0.0)?;

    assert_lands(&repaired, [2.0, 50.0]);
    Ok(())
}

/// A ratio between levels is an ordinary input to repair, not a mistake: the
/// nearest level is 1, where speed 22 is already feasible.
#[test]
fn an_off_level_point_is_repaired_onto_a_level() -> anyhow::Result<()> {
    let (_, region) = gearbox_region()?;

    let repaired = region.repair(&[1.2, 22.0], 0.0)?;

    assert_lands(&repaired, [1.0, 22.0]);
    Ok(())
}

/// With nothing continuous to move, a repair is a search of the lattice
/// alone: from `(1, 0)` the points with `n + m == 7` are `(7, 0)` at 0.6 of
/// the box, `(4, 3)` at about 0.42 and `(3, 4)` at about 0.45 — the nearest
/// is the answer, exactly, and the same every time.
#[test]
fn a_repair_of_discrete_variables_alone_is_the_nearest_lattice_point() -> anyhow::Result<()> {
    let system = system(
        vec![
            InputVariable::new_discrete("n", 0.0, 1.0, 10.0),
            InputVariable::new_discrete("m", 0.0, 1.0, 10.0),
        ],
        &["n + m == 7 +/- 0.1"],
    )?;
    let region = region(&system)?;

    let repaired = region.repair(&[1.0, 0.0], 0.0)?;
    let again = region.repair(&[1.0, 0.0], 0.0)?;

    assert_eq!(repaired, vec![4.0, 3.0]);
    assert_eq!(again, repaired);
    Ok(())
}

// --- Determinism ------------------------------------------------------------

/// The same generator state designs the same points, discrete draws and all.
#[test]
fn the_same_seed_designs_the_same_discrete_points() -> anyhow::Result<()> {
    let (variables, sources) = gearbox(5.0);
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let system = system(variables, &sources)?;

    let first = region(&system)?.sample(Mat::zeros(0, 0).as_ref(), 8, &mut seeded())?;
    let second = region(&system)?.sample(Mat::zeros(0, 0).as_ref(), 8, &mut seeded())?;

    assert_eq!(columns(&first), columns(&second));
    Ok(())
}
